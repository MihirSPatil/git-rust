use flate2::{Compression, write::ZlibEncoder};
use git_rust::{
    objects::{
        GitObjectKind, checkout_tree, initialize_repository, read_git_object,
        split_header_and_body, write_git_object, write_working_tree,
    },
    packfile::unpack_packfile_into_repo,
};
use hex::encode;
use std::{fs, io::Write, path::PathBuf};

fn temp_test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("git_rust_it_{}_{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn blob_roundtrip_matches_canonical_git_sha1() {
    // Canonical Git SHA-1 for "hello world\n" is 3b18e512dba79e4c8300dd08aeb37f8e728b8dad
    let workspace = temp_test_dir("blob_sha1");
    let git_dir = initialize_repository(&workspace, "refs/heads/main").unwrap();

    let sha = write_git_object(&git_dir, GitObjectKind::Blob, b"hello world\n", true).unwrap();
    let hex_sha = encode(sha);
    assert_eq!(hex_sha, "3b18e512dba79e4c8300dd08aeb37f8e728b8dad");

    let raw = read_git_object(&git_dir, &hex_sha).unwrap();
    let (header, body) = split_header_and_body(&raw).unwrap();
    assert_eq!(header, "blob 12");
    assert_eq!(body, b"hello world\n");

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn write_working_tree_and_checkout_tree_roundtrip() {
    let source_dir = temp_test_dir("tree_src");
    let dest_dir = temp_test_dir("tree_dst");
    let git_dir = initialize_repository(&source_dir, "refs/heads/main").unwrap();

    fs::create_dir_all(source_dir.join("nested")).unwrap();
    fs::write(source_dir.join("README.md"), b"# Demo\n").unwrap();
    fs::write(source_dir.join("nested/data.txt"), b"hello tree").unwrap();

    let root_entry = write_working_tree(&git_dir, &source_dir).unwrap().unwrap();
    let root_tree_sha = encode(root_entry.sha);

    checkout_tree(&git_dir, &root_tree_sha, &dest_dir).unwrap();

    assert_eq!(fs::read(dest_dir.join("README.md")).unwrap(), b"# Demo\n");
    assert_eq!(
        fs::read(dest_dir.join("nested/data.txt")).unwrap(),
        b"hello tree"
    );

    let _ = fs::remove_dir_all(&source_dir);
    let _ = fs::remove_dir_all(&dest_dir);
}

#[test]
fn checkout_tree_blocks_path_traversal_and_git_dir_overwrite() {
    let workspace = temp_test_dir("traversal_guard");
    let checkout_dir = workspace.join("out");
    let git_dir = initialize_repository(&workspace, "refs/heads/main").unwrap();

    let blob_sha = write_git_object(&git_dir, GitObjectKind::Blob, b"pwned", true).unwrap();

    // Craft a malicious tree entry trying to overwrite .git/hooks
    let mut malicious_tree = Vec::new();
    malicious_tree.extend_from_slice(b"100755 .git\0");
    malicious_tree.extend_from_slice(&blob_sha);

    let tree_sha = write_git_object(&git_dir, GitObjectKind::Tree, &malicious_tree, true).unwrap();
    let result = checkout_tree(&git_dir, &encode(tree_sha), &checkout_dir);
    assert!(result.is_err());

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn unpacks_packfile_with_base_blob_and_ofs_delta() {
    let workspace = temp_test_dir("pack_ofs_delta");
    let git_dir = initialize_repository(&workspace, "refs/heads/main").unwrap();

    let base_data = b"Hello, world!";
    let mut base_zlib = ZlibEncoder::new(Vec::new(), Compression::default());
    base_zlib.write_all(base_data).unwrap();
    let base_compressed = base_zlib.finish().unwrap();

    // Delta turning "Hello, world!" (13 bytes) into "Hello, Rust!" (12 bytes)
    let mut delta_payload = vec![13, 12, 0x91, 0, 7, 5];
    delta_payload.extend_from_slice(b"Rust!");
    let mut delta_zlib = ZlibEncoder::new(Vec::new(), Compression::default());
    delta_zlib.write_all(&delta_payload).unwrap();
    let delta_compressed = delta_zlib.finish().unwrap();

    let mut pack = Vec::new();
    pack.extend_from_slice(b"PACK");
    pack.extend_from_slice(&2u32.to_be_bytes()); // version 2
    pack.extend_from_slice(&2u32.to_be_bytes()); // 2 objects

    // Entry 1 at offset 12: type 3 (blob), size 13 -> (3 << 4) | 13 = 0x3d
    let entry1_offset = pack.len();
    pack.push(0x3d);
    pack.extend_from_slice(&base_compressed);

    // Entry 2: type 6 (ofs_delta), size = delta_payload.len() (11) -> (6 << 4) | 11 = 0x6b
    let entry2_offset = pack.len();
    let distance = (entry2_offset - entry1_offset) as u8;
    pack.push(0x6b);
    pack.push(distance);
    pack.extend_from_slice(&delta_compressed);

    let count = unpack_packfile_into_repo(&git_dir, &pack).unwrap();
    assert_eq!(count, 2);

    // Verify the delta-reconstructed blob ("Hello, Rust!") was written to .git/objects
    let expected_sha =
        encode(write_git_object(&git_dir, GitObjectKind::Blob, b"Hello, Rust!", false).unwrap());
    let unpacked_raw = read_git_object(&git_dir, &expected_sha).unwrap();
    let (_, unpacked_body) = split_header_and_body(&unpacked_raw).unwrap();
    assert_eq!(unpacked_body, b"Hello, Rust!");

    let _ = fs::remove_dir_all(&workspace);
}
