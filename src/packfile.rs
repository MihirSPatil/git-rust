use std::{
    collections::HashMap,
    fs,
    io::{Cursor, Read},
    path::Path,
    rc::Rc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use flate2::bufread::ZlibDecoder;
use hex::encode;
use reqwest::blocking as http;

use crate::objects::{
    GitObjectKind, checkout_tree, initialize_repository, read_git_object, split_header_and_body,
    validate_sha1_hex, write_git_object,
};

/// Upper bound (512 MiB) on any single unpacked Git object to guard against decompression bombs.
const MAX_UNPACKED_OBJECT_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug, Clone)]
struct RemoteHeadInfo {
    head_sha: String,
    default_branch_ref: String,
}

/// Encodes a payload line using Git's 4-hex-digit `pkt-line` framing (`0000` is a flush packet).
fn encode_pkt_line(payload: &str) -> Result<String> {
    if payload.is_empty() {
        return Ok(String::from("0000"));
    }

    let framed_length = payload.len() + 4;
    ensure!(
        framed_length <= 0xffff,
        "pkt-line payload exceeds maximum 65535-byte frame size"
    );

    Ok(format!("{:04x}{}", framed_length, payload))
}

/// Queries `<repo_url>/info/refs?service=git-upload-pack` to discover `HEAD` and its symref branch.
fn discover_remote_head(client: &http::Client, repo_url: &str) -> Result<RemoteHeadInfo> {
    let info_refs_url = format!(
        "{}/info/refs?service=git-upload-pack",
        repo_url.trim_end_matches('/')
    );

    let response_bytes = client
        .get(&info_refs_url)
        .send()
        .context("Failed to connect to remote repository")?
        .error_for_status()
        .context("Remote server returned an HTTP error during ref discovery")?
        .bytes()?;

    let mut remaining: &[u8] = &response_bytes;

    while remaining.len() >= 4 {
        let length_prefix = std::str::from_utf8(&remaining[..4])
            .context("Invalid UTF-8 in pkt-line length prefix")?;
        let packet_length = usize::from_str_radix(length_prefix, 16)
            .context("Invalid hex length in pkt-line frame")?;

        if packet_length == 0 {
            remaining = &remaining[4..];
            continue;
        }

        ensure!(
            packet_length >= 4 && packet_length <= remaining.len(),
            "Malformed pkt-line frame length {} (remaining bytes: {})",
            packet_length,
            remaining.len()
        );

        let payload = &remaining[4..packet_length];
        let line = std::str::from_utf8(payload)?.trim_end_matches('\n');

        if let Some((ref_part, capabilities_part)) = line.split_once('\0')
            && let Some(sha) = ref_part.strip_suffix(" HEAD")
        {
            validate_sha1_hex(sha)?;
            let default_branch_ref = capabilities_part
                .split_whitespace()
                .find_map(|cap| cap.strip_prefix("symref=HEAD:"))
                .unwrap_or("refs/heads/main")
                .to_string();

            return Ok(RemoteHeadInfo {
                head_sha: sha.to_string(),
                default_branch_ref,
            });
        }

        remaining = &remaining[packet_length..];
    }

    bail!("Remote repository did not advertise a HEAD reference")
}

/// Requests a packfile containing `want_sha` from `<repo_url>/git-upload-pack`.
fn fetch_packfile(client: &http::Client, repo_url: &str, want_sha: &str) -> Result<Vec<u8>> {
    let upload_pack_url = format!("{}/git-upload-pack", repo_url.trim_end_matches('/'));

    let mut request_body = String::new();
    request_body.push_str(&encode_pkt_line(&format!("want {}\n", want_sha))?);
    request_body.push_str(&encode_pkt_line("")?);
    request_body.push_str(&encode_pkt_line("done\n")?);

    let response = client
        .post(&upload_pack_url)
        .header("Content-Type", "application/x-git-upload-pack-request")
        .body(request_body)
        .send()
        .context("Failed to send git-upload-pack request")?
        .error_for_status()
        .context("Remote server returned an error while fetching packfile")?
        .bytes()?;

    Ok(response.to_vec())
}

/// Reads a standard little-endian base-128 varint from `buffer` starting at `*offset`.
fn read_varint_le(buffer: &[u8], offset: &mut usize) -> Result<usize> {
    let mut value: usize = 0;
    let mut bit_shift: u32 = 0;

    loop {
        let byte = *buffer
            .get(*offset)
            .ok_or_else(|| anyhow!("Unexpected end of delta stream while reading varint"))?;
        *offset += 1;

        let seven_bit_chunk = ((byte & 0x7f) as usize)
            .checked_shl(bit_shift)
            .ok_or_else(|| anyhow!("Delta varint overflow"))?;
        value = value
            .checked_add(seven_bit_chunk)
            .ok_or_else(|| anyhow!("Delta varint overflow"))?;

        if byte < 128 {
            break;
        }
        bit_shift += 7;
    }

    Ok(value)
}

/// Reconstructs a target object by applying Git binary delta instructions onto `base_bytes`.
fn apply_delta_patch(base_bytes: &[u8], delta_bytes: &[u8]) -> Result<Vec<u8>> {
    let mut cursor = 0;
    let expected_base_size = read_varint_le(delta_bytes, &mut cursor)?;
    let target_size = read_varint_le(delta_bytes, &mut cursor)?;

    ensure!(
        base_bytes.len() == expected_base_size,
        "Delta base size mismatch: expected {} bytes, got {}",
        expected_base_size,
        base_bytes.len()
    );
    ensure!(
        target_size <= MAX_UNPACKED_OBJECT_BYTES,
        "Delta target size ({} bytes) exceeds safety cap",
        target_size
    );

    let mut reconstructed = Vec::with_capacity(target_size);

    while cursor < delta_bytes.len() {
        let opcode = delta_bytes[cursor];
        cursor += 1;

        if opcode >= 128 {
            // Copy instruction: bit flags determine which offset and size bytes follow.
            let mut copy_offset: usize = 0;
            let mut copy_length: usize = 0;

            for byte_idx in 0..4 {
                if (opcode & (1 << byte_idx)) != 0 {
                    let b = *delta_bytes
                        .get(cursor)
                        .ok_or_else(|| anyhow!("Truncated delta copy offset"))?;
                    cursor += 1;
                    copy_offset |= (b as usize) << (byte_idx * 8);
                }
            }

            for byte_idx in 0..3 {
                if (opcode & (1 << (byte_idx + 4))) != 0 {
                    let b = *delta_bytes
                        .get(cursor)
                        .ok_or_else(|| anyhow!("Truncated delta copy length"))?;
                    cursor += 1;
                    copy_length |= (b as usize) << (byte_idx * 8);
                }
            }

            if copy_length == 0 {
                copy_length = 65_536;
            }

            let copy_end = copy_offset
                .checked_add(copy_length)
                .ok_or_else(|| anyhow!("Delta copy range overflow"))?;
            let slice = base_bytes
                .get(copy_offset..copy_end)
                .ok_or_else(|| anyhow!("Delta copy out of bounds on base object"))?;
            reconstructed.extend_from_slice(slice);
        } else if opcode > 0 {
            // Insert instruction: copy `opcode` literal bytes from the delta stream.
            let insert_length = opcode as usize;
            let insert_end = cursor
                .checked_add(insert_length)
                .ok_or_else(|| anyhow!("Delta insert range overflow"))?;
            let literal_slice = delta_bytes
                .get(cursor..insert_end)
                .ok_or_else(|| anyhow!("Truncated delta insert payload"))?;
            reconstructed.extend_from_slice(literal_slice);
            cursor = insert_end;
        } else {
            bail!("Invalid 0x00 opcode in delta instruction stream");
        }
    }

    ensure!(
        reconstructed.len() == target_size,
        "Delta output length mismatch: expected {}, got {}",
        target_size,
        reconstructed.len()
    );

    Ok(reconstructed)
}

/// Reads Git's big-endian `+1` offset encoding used by `OFS_DELTA` packfile entries.
fn read_ofs_delta_distance(cursor: &mut Cursor<&[u8]>) -> Result<u64> {
    let mut byte = [0u8; 1];
    cursor.read_exact(&mut byte)?;
    let mut distance = (byte[0] % 128) as u64;

    while byte[0] >= 128 {
        cursor.read_exact(&mut byte)?;
        distance = distance
            .checked_add(1)
            .and_then(|d| d.checked_mul(128))
            .and_then(|d| d.checked_add((byte[0] % 128) as u64))
            .ok_or_else(|| anyhow!("OFS_DELTA negative offset overflow"))?;
    }

    Ok(distance)
}

/// Unpacks all objects (including `OFS_DELTA` and `REF_DELTA`) from `pack_stream` into `git_dir`.
pub fn unpack_packfile_into_repo(git_dir: &Path, pack_stream: &[u8]) -> Result<usize> {
    let pack_start = pack_stream
        .windows(4)
        .position(|window| window == b"PACK")
        .ok_or_else(|| anyhow!("PACK header signature not found in server response"))?;

    let mut cursor = Cursor::new(&pack_stream[pack_start..]);

    let mut magic = [0u8; 4];
    cursor.read_exact(&mut magic)?;

    let mut version_bytes = [0u8; 4];
    cursor.read_exact(&mut version_bytes)?;
    let pack_version = u32::from_be_bytes(version_bytes);
    ensure!(
        pack_version == 2 || pack_version == 3,
        "Unsupported packfile version: {}",
        pack_version
    );

    let mut object_count_bytes = [0u8; 4];
    cursor.read_exact(&mut object_count_bytes)?;
    let object_count = u32::from_be_bytes(object_count_bytes) as usize;

    // O(1) lookup tables for delta base resolution by packfile offset or SHA-1 hex.
    let mut unpacked_by_offset: HashMap<u64, Rc<(GitObjectKind, Vec<u8>)>> =
        HashMap::with_capacity(object_count);
    let mut unpacked_by_sha: HashMap<String, Rc<(GitObjectKind, Vec<u8>)>> =
        HashMap::with_capacity(object_count);

    for _ in 0..object_count {
        let entry_offset = cursor.position();

        let mut header_byte = [0u8; 1];
        cursor.read_exact(&mut header_byte)?;

        let mut has_more_size_bytes = header_byte[0] >= 128;
        let raw_type_bits = (header_byte[0] % 128) / 16;
        let mut uncompressed_size = (header_byte[0] % 16) as usize;
        let mut bit_shift: u32 = 4;

        while has_more_size_bytes {
            cursor.read_exact(&mut header_byte)?;
            has_more_size_bytes = header_byte[0] >= 128;
            let chunk = ((header_byte[0] % 128) as usize)
                .checked_shl(bit_shift)
                .ok_or_else(|| anyhow!("Packfile object size overflow"))?;
            uncompressed_size = uncompressed_size
                .checked_add(chunk)
                .ok_or_else(|| anyhow!("Packfile object size overflow"))?;
            bit_shift += 7;
        }

        ensure!(
            uncompressed_size <= MAX_UNPACKED_OBJECT_BYTES,
            "Packfile object size ({} bytes) exceeds safety limit",
            uncompressed_size
        );

        let mut ofs_base_offset: Option<u64> = None;
        let mut ref_base_sha: Option<String> = None;
        let base_kind: Option<GitObjectKind> = match raw_type_bits {
            1 => Some(GitObjectKind::Commit),
            2 => Some(GitObjectKind::Tree),
            3 => Some(GitObjectKind::Blob),
            4 => Some(GitObjectKind::Tag),
            6 => {
                let distance = read_ofs_delta_distance(&mut cursor)?;
                let target_offset = entry_offset
                    .checked_sub(distance)
                    .ok_or_else(|| anyhow!("OFS_DELTA points before start of packfile"))?;
                ofs_base_offset = Some(target_offset);
                None
            }
            7 => {
                let mut base_sha_bytes = [0u8; 20];
                cursor.read_exact(&mut base_sha_bytes)?;
                ref_base_sha = Some(encode(base_sha_bytes));
                None
            }
            other => bail!("Unknown packfile object type code: {}", other),
        };

        let mut decoder = ZlibDecoder::new(&mut cursor);
        let mut decompressed_data = Vec::with_capacity(uncompressed_size);
        decoder.read_to_end(&mut decompressed_data)?;

        ensure!(
            decompressed_data.len() == uncompressed_size,
            "Packfile object size mismatch: header declared {} bytes, decompressed {} bytes",
            uncompressed_size,
            decompressed_data.len()
        );

        let (resolved_kind, resolved_body) = if let Some(target_offset) = ofs_base_offset {
            let base = unpacked_by_offset
                .get(&target_offset)
                .ok_or_else(|| anyhow!("OFS_DELTA base at offset {} not found", target_offset))?;
            (base.0, apply_delta_patch(&base.1, &decompressed_data)?)
        } else if let Some(target_sha) = ref_base_sha {
            let base = unpacked_by_sha
                .get(&target_sha)
                .ok_or_else(|| anyhow!("REF_DELTA base SHA {} not found", target_sha))?;
            (base.0, apply_delta_patch(&base.1, &decompressed_data)?)
        } else {
            let kind = base_kind.ok_or_else(|| anyhow!("Missing object kind"))?;
            (kind, decompressed_data)
        };

        let sha_bytes = write_git_object(git_dir, resolved_kind, &resolved_body, true)?;
        let sha_hex = encode(sha_bytes);

        let shared_object = Rc::new((resolved_kind, resolved_body));
        unpacked_by_offset.insert(entry_offset, Rc::clone(&shared_object));
        unpacked_by_sha.insert(sha_hex, shared_object);
    }

    Ok(object_count)
}

/// Clones a public HTTP/HTTPS Git repository into `destination_dir`.
pub fn clone_repository(repo_url: &str, destination_dir: &Path) -> Result<()> {
    ensure!(
        repo_url.starts_with("https://") || repo_url.starts_with("http://"),
        "Only http:// and https:// repository URLs are supported"
    );

    if destination_dir.exists() && destination_dir.read_dir()?.next().is_some() {
        bail!(
            "Destination path '{}' already exists and is not an empty directory",
            destination_dir.display()
        );
    }

    let client = http::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("git-rust/0.1.0")
        .build()?;

    let remote_head = discover_remote_head(&client, repo_url)?;
    let git_dir = initialize_repository(destination_dir, &remote_head.default_branch_ref)?;

    let branch_ref_path = git_dir.join(&remote_head.default_branch_ref);
    if let Some(parent) = branch_ref_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&branch_ref_path, format!("{}\n", remote_head.head_sha))?;

    let pack_response = fetch_packfile(&client, repo_url, &remote_head.head_sha)?;
    unpack_packfile_into_repo(&git_dir, &pack_response)?;

    let head_commit_raw = read_git_object(&git_dir, &remote_head.head_sha)?;
    let (_, commit_body) = split_header_and_body(&head_commit_raw)?;
    let commit_text = std::str::from_utf8(commit_body)?;

    let root_tree_sha = commit_text
        .lines()
        .find_map(|line| line.strip_prefix("tree "))
        .ok_or_else(|| {
            anyhow!(
                "Root tree SHA not found in HEAD commit {}",
                remote_head.head_sha
            )
        })?;

    checkout_tree(&git_dir, root_tree_sha, destination_dir)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_pkt_lines_and_flush_packets() {
        assert_eq!(encode_pkt_line("").unwrap(), "0000");
        assert_eq!(
            encode_pkt_line("want 3b0466d22854e57bf9ad3ccf82008a2d3f199550\n").unwrap(),
            "0032want 3b0466d22854e57bf9ad3ccf82008a2d3f199550\n"
        );
    }

    #[test]
    fn applies_delta_copy_and_insert_instructions() {
        let base = b"Hello, world!";
        // Base len = 13, Target len = 12
        // Copy 7 bytes from offset 0 ("Hello, ") -> 0x91, 0, 7
        // Insert 5 bytes ("Rust!") -> 0x05, b"Rust!"
        let mut delta = vec![13, 12, 0x91, 0, 7, 5];
        delta.extend_from_slice(b"Rust!");

        let patched = apply_delta_patch(base, &delta).unwrap();
        assert_eq!(patched, b"Hello, Rust!");
    }

    #[test]
    fn rejects_out_of_bounds_delta_copy() {
        let base = b"short";
        let delta = vec![5, 10, 0x91, 0, 10];
        assert!(apply_delta_patch(base, &delta).is_err());
    }

    #[test]
    fn decodes_ofs_delta_plus_one_distance() {
        // Single-byte distance: 42
        let mut single = Cursor::new(&[42u8][..]);
        assert_eq!(read_ofs_delta_distance(&mut single).unwrap(), 42);

        // Multi-byte distance: 0x80 (0 with continuation), 0x05 -> ((0 + 1) * 128) + 5 = 133
        let mut multi = Cursor::new(&[0x80u8, 0x05u8][..]);
        assert_eq!(read_ofs_delta_distance(&mut multi).unwrap(), 133);
    }
}
