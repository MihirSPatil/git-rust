use std::{
    fs,
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use anyhow::{Context, Result, anyhow, bail, ensure};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use hex::encode;
use sha1::{Digest, Sha1};

/// Supported Git loose object types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitObjectKind {
    Blob,
    Tree,
    Commit,
    Tag,
}

impl GitObjectKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            GitObjectKind::Blob => "blob",
            GitObjectKind::Tree => "tree",
            GitObjectKind::Commit => "commit",
            GitObjectKind::Tag => "tag",
        }
    }
}

impl FromStr for GitObjectKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "blob" => Ok(GitObjectKind::Blob),
            "tree" => Ok(GitObjectKind::Tree),
            "commit" => Ok(GitObjectKind::Commit),
            "tag" => Ok(GitObjectKind::Tag),
            other => bail!("Unsupported Git object type: '{}'", other),
        }
    }
}

/// A single entry inside a Git tree object (`<mode> <name>\0<20-byte-sha>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub mode: String,
    pub name: String,
    pub sha: [u8; 20],
}

/// Validates that a string is a well-formed 40-character hexadecimal SHA-1 hash.
pub fn validate_sha1_hex(hex_hash: &str) -> Result<()> {
    ensure!(
        hex_hash.len() == 40 && hex_hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid SHA-1 hash: '{}'",
        hex_hash
    );
    Ok(())
}

/// Ensures a tree entry filename cannot traverse directories (`..`) or overwrite `.git`.
fn is_safe_entry_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.eq_ignore_ascii_case(".git")
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

/// Computes `.git/objects/<first-2-chars>/<remaining-38-chars>` for a given SHA-1 hex string.
fn object_file_path(git_dir: &Path, hex_hash: &str) -> Result<PathBuf> {
    validate_sha1_hex(hex_hash)?;
    let (directory_prefix, file_name) = hex_hash.split_at(2);
    Ok(git_dir
        .join("objects")
        .join(directory_prefix)
        .join(file_name))
}

/// Initializes a `.git` directory structure inside `target_dir`.
pub fn initialize_repository(target_dir: &Path, head_ref: &str) -> Result<PathBuf> {
    ensure!(
        !head_ref.contains("..") && !head_ref.contains('\0'),
        "Unsafe HEAD reference: '{}'",
        head_ref
    );

    let git_dir = target_dir.join(".git");
    fs::create_dir_all(git_dir.join("objects"))
        .context("Failed to create .git/objects directory")?;
    fs::create_dir_all(git_dir.join("refs/heads"))
        .context("Failed to create .git/refs/heads directory")?;
    fs::write(git_dir.join("HEAD"), format!("ref: {}\n", head_ref))
        .context("Failed to write .git/HEAD")?;

    Ok(git_dir)
}

/// Hashes `<type> <size>\0<body>` with SHA-1 and optionally writes the zlib-compressed object to disk.
pub fn write_git_object(
    git_dir: &Path,
    kind: GitObjectKind,
    body: &[u8],
    persist_to_disk: bool,
) -> Result<[u8; 20]> {
    let header = format!("{} {}\0", kind.as_str(), body.len());

    let mut hasher = Sha1::new();
    hasher.update(header.as_bytes());
    hasher.update(body);
    let sha_bytes: [u8; 20] = hasher.finalize().into();

    if persist_to_disk {
        let destination = object_file_path(git_dir, &encode(sha_bytes))?;
        if let Some(parent_dir) = destination.parent() {
            fs::create_dir_all(parent_dir)?;
        }

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(header.as_bytes())?;
        encoder.write_all(body)?;
        let compressed_payload = encoder.finish()?;

        fs::write(&destination, compressed_payload)
            .with_context(|| format!("Failed to write Git object to {}", destination.display()))?;
    }

    Ok(sha_bytes)
}

/// Reads and zlib-decompresses a loose Git object from `.git/objects`.
pub fn read_git_object(git_dir: &Path, hex_hash: &str) -> Result<Vec<u8>> {
    let path = object_file_path(git_dir, hex_hash)?;
    let file = fs::File::open(&path)
        .with_context(|| format!("Git object {} not found at {}", hex_hash, path.display()))?;

    let mut decoder = ZlibDecoder::new(BufReader::new(file));
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .with_context(|| format!("Failed to decompress Git object {}", hex_hash))?;

    Ok(decompressed)
}

/// Splits `<type> <size>\0<body>` into its UTF-8 header string and raw body bytes.
pub fn split_header_and_body(raw_object: &[u8]) -> Result<(&str, &[u8])> {
    let null_index = raw_object
        .iter()
        .position(|&byte| byte == b'\0')
        .ok_or_else(|| anyhow!("Corrupt Git object: missing NUL header delimiter"))?;

    let header = std::str::from_utf8(&raw_object[..null_index])
        .context("Git object header is not valid UTF-8")?;
    let body = &raw_object[null_index + 1..];

    Ok((header, body))
}

/// Parses the binary payload of a Git tree object into structured `TreeEntry` records.
pub fn parse_tree_entries(tree_body: &[u8]) -> Result<Vec<TreeEntry>> {
    let mut remaining = tree_body;
    let mut entries = Vec::new();

    while !remaining.is_empty() {
        let space_index = remaining
            .iter()
            .position(|&byte| byte == b' ')
            .ok_or_else(|| anyhow!("Corrupt tree object: missing space after file mode"))?;
        let mode = std::str::from_utf8(&remaining[..space_index])?.to_string();
        remaining = &remaining[space_index + 1..];

        let null_index = remaining
            .iter()
            .position(|&byte| byte == b'\0')
            .ok_or_else(|| {
                anyhow!("Corrupt tree object: missing NUL terminator after entry name")
            })?;
        let name = std::str::from_utf8(&remaining[..null_index])?.to_string();
        remaining = &remaining[null_index + 1..];

        let raw_sha = remaining
            .get(..20)
            .ok_or_else(|| anyhow!("Corrupt tree object: truncated 20-byte SHA"))?;
        let mut sha = [0u8; 20];
        sha.copy_from_slice(raw_sha);
        remaining = &remaining[20..];

        entries.push(TreeEntry { mode, name, sha });
    }

    Ok(entries)
}

/// Recursively hashes files (blobs) and directories (trees) from `current_path` into `git_dir`.
pub fn write_working_tree(git_dir: &Path, current_path: &Path) -> Result<Option<TreeEntry>> {
    if current_path.is_file() {
        let file_contents = fs::read(current_path)?;
        let blob_sha = write_git_object(git_dir, GitObjectKind::Blob, &file_contents, true)?;

        #[cfg(unix)]
        let mode = if current_path.metadata()?.permissions().mode() & 0o111 != 0 {
            "100755"
        } else {
            "100644"
        };

        #[cfg(not(unix))]
        let mode = "100644";

        let file_name = current_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("Non-UTF-8 filename encountered"))?
            .to_string();

        return Ok(Some(TreeEntry {
            mode: mode.to_string(),
            name: file_name,
            sha: blob_sha,
        }));
    }

    if current_path.is_dir() {
        let mut child_entries = Vec::new();

        for dir_entry in fs::read_dir(current_path)? {
            let child_path = dir_entry?.path();
            if child_path.file_name().and_then(|name| name.to_str()) == Some(".git") {
                continue;
            }
            if let Some(entry) = write_working_tree(git_dir, &child_path)? {
                child_entries.push(entry);
            }
        }

        if child_entries.is_empty() {
            return Ok(None);
        }

        // Git requires tree entries to be sorted lexicographically by name.
        child_entries.sort_by(|a, b| a.name.cmp(&b.name));

        let mut tree_body = Vec::new();
        for entry in &child_entries {
            tree_body.extend_from_slice(entry.mode.as_bytes());
            tree_body.push(b' ');
            tree_body.extend_from_slice(entry.name.as_bytes());
            tree_body.push(b'\0');
            tree_body.extend_from_slice(&entry.sha);
        }

        let tree_sha = write_git_object(git_dir, GitObjectKind::Tree, &tree_body, true)?;
        let dir_name = current_path
            .file_name()
            .unwrap_or_default()
            .to_str()
            .ok_or_else(|| anyhow!("Non-UTF-8 directory name encountered"))?
            .to_string();

        return Ok(Some(TreeEntry {
            mode: "40000".to_string(),
            name: dir_name,
            sha: tree_sha,
        }));
    }

    Ok(None)
}

/// Recursively reconstructs directories and files in `destination_dir` from `tree_sha`.
pub fn checkout_tree(git_dir: &Path, tree_sha: &str, destination_dir: &Path) -> Result<()> {
    fs::create_dir_all(destination_dir)?;

    let raw_tree = read_git_object(git_dir, tree_sha)?;
    let (header, tree_body) = split_header_and_body(&raw_tree)?;
    ensure!(
        header.starts_with("tree "),
        "Expected a tree object for {}, found '{}'",
        tree_sha,
        header
    );

    let entries = parse_tree_entries(tree_body)?;

    for entry in entries {
        ensure!(
            is_safe_entry_name(&entry.name),
            "Refusing to check out unsafe tree entry '{}'",
            entry.name
        );

        let entry_path = destination_dir.join(&entry.name);
        let entry_sha_hex = encode(entry.sha);

        if entry.mode == "40000" {
            checkout_tree(git_dir, &entry_sha_hex, &entry_path)?;
        } else {
            let raw_blob = read_git_object(git_dir, &entry_sha_hex)?;
            let (_, blob_body) = split_header_and_body(&raw_blob)?;
            fs::write(&entry_path, blob_body)?;

            #[cfg(unix)]
            if entry.mode == "100755" {
                fs::set_permissions(&entry_path, fs::Permissions::from_mode(0o755))?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_object_kinds_and_rejects_unknown() {
        assert_eq!(
            "blob".parse::<GitObjectKind>().unwrap(),
            GitObjectKind::Blob
        );
        assert_eq!(
            "tree".parse::<GitObjectKind>().unwrap(),
            GitObjectKind::Tree
        );
        assert_eq!(
            "commit".parse::<GitObjectKind>().unwrap(),
            GitObjectKind::Commit
        );
        assert_eq!("tag".parse::<GitObjectKind>().unwrap(), GitObjectKind::Tag);
        assert!("bogus".parse::<GitObjectKind>().is_err());
    }

    #[test]
    fn rejects_unsafe_tree_entry_names() {
        assert!(is_safe_entry_name("README.md"));
        assert!(is_safe_entry_name("src"));
        assert!(!is_safe_entry_name(""));
        assert!(!is_safe_entry_name("."));
        assert!(!is_safe_entry_name(".."));
        assert!(!is_safe_entry_name(".git"));
        assert!(!is_safe_entry_name(".GIT"));
        assert!(!is_safe_entry_name("../evil"));
        assert!(!is_safe_entry_name("sub\\file"));
    }

    #[test]
    fn parses_binary_tree_entries() {
        let mut raw_tree = Vec::new();
        raw_tree.extend_from_slice(b"100644 hello.txt\0");
        raw_tree.extend_from_slice(&[0xabu8; 20]);

        let entries = parse_tree_entries(&raw_tree).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].mode, "100644");
        assert_eq!(entries[0].name, "hello.txt");
        assert_eq!(entries[0].sha, [0xabu8; 20]);
    }
}
