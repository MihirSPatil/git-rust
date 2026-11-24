use git_rust::{
    objects::{
        GitObjectKind, initialize_repository, is_safe_entry_name, parse_tree_entries,
        read_git_object, split_header_and_body, write_git_object,
    },
    packfile::{apply_delta_patch, encode_pkt_line},
};
use hex::encode;
use std::{fs, path::PathBuf};

fn temp_test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("git_rust_test_{}_{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn test_blob_roundtrip_matches_official_git_sha1() {
    // Official Git SHA-1 for "hello world\n" is 3b18e512dba79e4c8300dd08aeb37f8e728b8dad
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
fn test_pkt_line_framing() {
    assert_eq!(encode_pkt_line("").unwrap(), "0000");
    assert_eq!(
        encode_pkt_line("want 3b0466d22854e57bf9ad3ccf82008a2d3f199550\n").unwrap(),
        "0032want 3b0466d22854e57bf9ad3ccf82008a2d3f199550\n"
    );
}

#[test]
fn test_delta_copy_and_insert() {
    let base = b"Hello, world!";
    // Base len = 13 (0x0d), Target len = 12 (0x0c)
    // Copy 7 bytes from offset 0 ("Hello, ") -> opcode 0x80 | 0x01 (offset byte) | 0x10 (size byte) = 0x91, offset=0, size=7
    // Insert 5 bytes ("Rust!") -> opcode 0x05, b"Rust!"
    let mut delta = vec![13, 12, 0x91, 0, 7, 5];
    delta.extend_from_slice(b"Rust!");

    let patched = apply_delta_patch(base, &delta).unwrap();
    assert_eq!(patched, b"Hello, Rust!");
}

#[test]
fn test_tree_parsing_and_path_traversal_guard() {
    assert!(is_safe_entry_name("README.md"));
    assert!(is_safe_entry_name("src"));
    assert!(!is_safe_entry_name(".."));
    assert!(!is_safe_entry_name(".git"));
    assert!(!is_safe_entry_name("../evil"));

    let mut raw_tree = Vec::new();
    raw_tree.extend_from_slice(b"100644 hello.txt\0");
    raw_tree.extend_from_slice(&[0xabu8; 20]);

    let entries = parse_tree_entries(&raw_tree).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].mode, "100644");
    assert_eq!(entries[0].name, "hello.txt");
    assert_eq!(entries[0].sha, [0xabu8; 20]);
}
