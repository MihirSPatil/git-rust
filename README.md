# git-rust — A From-Scratch Git Client in Rust

A clean, modular, zero-`unwrap` implementation of Git's core plumbing commands and Smart HTTP `clone` protocol written in Rust. Built to understand Git from the bytes up: content-addressable SHA-1 storage, zlib compression, binary tree serialization, `pkt-line` wire framing, packfile varint decoding, `OFS_DELTA` / `REF_DELTA` binary patching, and recursive working-tree checkout.

---

## Engineering Highlights & Systems Design

- **Wide & Shallow Module Architecture:** Organized into flat, self-contained domain modules (`objects.rs` and `packfile.rs`) with narrow public APIs, private low-level parsers, and co-located unit tests—optimized for both human readability and AI coding agents (see [`AGENTS.md`](AGENTS.md)).
- **Zero-Panic Error Propagation:** Replaced all `.unwrap()` calls with structured `anyhow::Result` error chains across network, file I/O, UTF-8 decoding, and binary parsing boundaries.
- **Memory & Algorithmic Efficiency:** Resolves `OFS_DELTA` and `REF_DELTA` packfile dependencies in **$O(1)$** using dual `HashMap` indexes backed by `Rc<(GitObjectKind, Vec<u8>)>`, avoiding duplicate buffer allocations in memory and pre-allocating target buffers from delta varint headers.
- **Security Hardening:** Guards against path-traversal and `.git/hooks` overwrite attacks during working-tree checkout, enforces a 512 MiB decompression-bomb cap on packfile entries, and uses checked arithmetic (`checked_add`, `checked_shl`, `checked_sub`) against malformed varints.
- **Cross-Platform Portability:** Isolates POSIX executable mode handling (`100755`) behind `#[cfg(unix)]` attributes and uses OS-agnostic `PathBuf` joins so the crate compiles cleanly on Linux, macOS, and Windows.
- **Spec-Verified Against Official Git:** Validated loose object storage, tree sorting, packfile unpacking, and working-tree checkouts directly with `git fsck --full`, `git ls-tree`, and `git status`.

---

## Supported Commands

| Command | What It Does |
| :--- | :--- |
| `git-rust init` | Initializes a `.git` repository (`objects/`, `refs/heads/`, and `HEAD`). |
| `git-rust hash-object [-w] <file>` | Computes the Git SHA-1 hash of a file and optionally writes it to `.git/objects/`. |
| `git-rust cat-file -p <sha>` | Decompresses a loose object from `.git/objects/` and prints its body. |
| `git-rust write-tree` | Recursively hashes the current working directory into `blob` and `tree` objects. |
| `git-rust ls-tree [--name-only] <sha>` | Parses a binary tree object and lists its entries. |
| `git-rust commit-tree <tree> [-p <parent>] -m <msg>` | Creates and stores a signed-format commit object pointing to `<tree>`. |
| `git-rust clone <repo-url> [target-dir]` | Clones a public HTTP/HTTPS repository over Git's Smart Protocol, unpacks deltas, and checks out the working tree. |

---

## Project Architecture

The codebase follows a **wide and shallow** structure—keeping related domain logic, private binary helpers, and unit tests together in two core modules rather than scattering code across nested folders:

```text
├── Cargo.toml                  # Builds the `git-rust` library crate and CLI binary
├── AGENTS.md                   # Architecture & invariant guide for AI coding agents
├── src/
│   ├── lib.rs                  # Public library exports (`objects` and `packfile`)
│   ├── main.rs                 # Thin CLI argument parser and subcommand dispatch
│   ├── objects.rs              # Loose object I/O, SHA-1 hashing, tree serialization, safe checkout + unit tests
│   └── packfile.rs             # Smart HTTP discovery, pkt-line framing, packfile & delta unpacking + unit tests
├── tests/
│   └── integration_tests.rs    # End-to-end round-trip, security & packfile tests (`cargo test`)
└── docs/
    └── INTERNALS.md            # Bit-level protocol & binary encoding reference
```

---

## Installation & Usage

### Prerequisites
- **Rust toolchain** (`cargo` / `rustc` 1.85+ or 1.96+ as configured in `Cargo.toml`)
- **Official `git` CLI** (optional, for cross-verifying generated repositories)

### 1. Run directly with Cargo
```bash
# Clone any public GitHub repository
cargo run --release --bin git-rust -- clone https://github.com/octocat/Hello-World /tmp/hello-world

# Inspect the cloned directory
cd /tmp/hello-world
git status
git log -p
```

### 2. Install `git-rust` globally on your machine
```bash
cargo install --path .
```
Cargo compiles the optimized release binary into `~/.cargo/bin/git-rust`, allowing you to run `git-rust` from any folder:
```bash
mkdir /tmp/my-repo && cd /tmp/my-repo
git-rust init
echo "hello rust" > hello.txt
git-rust hash-object -w hello.txt
git-rust write-tree
```

### 3. Use as a Rust library in another project
Add this repository to another project's `Cargo.toml`:
```toml
[dependencies]
git-rust = { git = "https://github.com/MihirSPatil/git-rust" }
```
Then call `clone_repository` or `write_git_object` directly from Rust:
```rust
use std::path::Path;
use git_rust::packfile::clone_repository;

fn main() -> anyhow::Result<()> {
    clone_repository("https://github.com/octocat/Hello-World", Path::new("./hello-world"))?;
    Ok(())
}
```

---

## How to Build Something Like This Yourself (Step-by-Step Guide)

If you want to build your own Git client from scratch, implement it in these 7 incremental milestones:

### Milestone 1: Repository Initialization (`init`)
A Git repository is just a `.git` folder with three things:
- `.git/objects/` — content-addressable object database
- `.git/refs/heads/` — branch pointers
- `.git/HEAD` — a text file containing `ref: refs/heads/main\n`

### Milestone 2: Blobs & Content-Addressable Storage (`hash-object`, `cat-file`)
Git never stores raw files by filename. Every object is formatted as:
```text
"<type> <byte-length>\0<raw-bytes>"
```
1. Compute the **SHA-1** hash of that combined header + body.
2. Compress the combined header + body using **zlib** (`flate2`).
3. Write the compressed bytes to `.git/objects/<first-2-hex-chars>/<remaining-38-hex-chars>`.

### Milestone 3: Binary Tree Objects (`ls-tree`, `write-tree`)
A `tree` object represents a directory. Its body is a concatenated sequence of entries (with **no** newline separators between them):
```text
"<mode> <entry-name>\0<20-byte-raw-binary-sha>"
```
- Regular files use mode `100644`, executables use `100755`, and subdirectories use `40000`.
- Entries in a tree **must** be sorted lexicographically by name before hashing, or your tree SHA will differ from official Git's.

### Milestone 4: Commit Objects (`commit-tree`)
A `commit` object has a plain UTF-8 text body:
```text
tree <40-char-tree-sha>
parent <40-char-parent-sha>
author Name <email> <unix-timestamp> +0000
committer Name <email> <unix-timestamp> +0000

Commit message
```

### Milestone 5: Smart HTTP Discovery & `pkt-line` Framing
To clone over HTTPS:
1. Send `GET <repo_url>/info/refs?service=git-upload-pack`.
2. Parse `pkt-line` frames—each line starts with a **4-character hex length** (including the 4 length bytes themselves), and `0000` is a flush packet.
3. Extract the `HEAD` commit SHA and the `symref=HEAD:refs/heads/<branch>` capability.
4. Send `POST <repo_url>/git-upload-pack` with `Content-Type: application/x-git-upload-pack-request` and body:
   ```text
   0032want <head-sha>\n
   0000
   0009done\n
   ```

### Milestone 6: Packfile Streaming & Delta Reconstruction (`OFS_DELTA` / `REF_DELTA`)
The server responds with a binary `PACK` stream:
1. Read the 12-byte header: `"PACK"` magic, 4-byte version (`2`), and 4-byte big-endian object count.
2. Each entry starts with a variable-length header byte:
   - Bit 7 (`>= 128`): continuation flag (`1` = more size bytes follow)
   - Bits 6..4 (`(byte % 128) / 16`): object type (`1`=commit, `2`=tree, `3`=blob, `4`=tag, `6`=`ofs_delta`, `7`=`ref_delta`)
   - Bits 3..0 (`byte % 16`): bottom 4 bits of uncompressed size
3. Decompress the following zlib stream. Because zlib is self-terminating, `ZlibDecoder` stops at the exact start of the next object.
4. For `ofs_delta` (negative byte offset in the packfile) and `ref_delta` (20-byte base SHA-1), look up the base object in `HashMap`s ($O(1)$) and run the delta copy/insert state machine to reconstruct the target object.

### Milestone 7: Recursive Working-Tree Checkout
Read the `HEAD` commit object, extract its `tree <sha>` line, and recursively walk every `TreeEntry` into the target folder—creating subdirectories for mode `40000` and writing blob contents (plus `0o755` permissions on Unix) for files.

---

## Verification & Testing

### 1. Run Automated Unit & Integration Tests
```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```
Runs both co-located unit tests inside `src/objects.rs` and `src/packfile.rs` and end-to-end tests in `tests/integration_tests.rs`.

### 2. Cross-Verify Object Creation Against Official `git`
```bash
mkdir -p /tmp/git-rust-verify && cd /tmp/git-rust-verify

cargo run --manifest-path /path/to/git-rust/Cargo.toml -- init
mkdir -p scooby/dooby
echo "scooby snacks" > scooby/dooby/doo

TREE_SHA=$(cargo run --manifest-path /path/to/git-rust/Cargo.toml -- write-tree)
COMMIT_SHA=$(cargo run --manifest-path /path/to/git-rust/Cargo.toml -- commit-tree "$TREE_SHA" -m "Initial commit")

git fsck --full
git ls-tree -r "$TREE_SHA"
git cat-file -p "$COMMIT_SHA"
```

### 3. End-to-End `clone` Verification Against Real GitHub Repositories
```bash
rm -rf /tmp/test-clone
cargo run --release --manifest-path /path/to/git-rust/Cargo.toml -- clone https://github.com/codecrafters-io/git-sample-1 /tmp/test-clone

cd /tmp/test-clone
git fsck --full          # Verifies every unpacked object and delta in .git/objects
git status               # Reports: "On branch master / nothing to commit, working tree clean"
git log --stat           # Verifies full commit history and checked-out files
```

---

## Deep-Dive Technical Reference

For a detailed walkthrough of Git's binary object formats, `pkt-line` framing, packfile bit-packing, and delta patching state machines, see [`docs/INTERNALS.md`](docs/INTERNALS.md).

---

## Acknowledgments & Attribution

- Inspired by the [**Build Your Own Git**](https://codecrafters.io/challenges/git) systems-programming challenge on [CodeCrafters](https://codecrafters.io), which provided the initial problem milestones and protocol specifications.
- Core implementation, wide-and-shallow library/CLI architecture (`objects.rs`, `packfile.rs`), $O(1)$ `HashMap` + `Rc` delta resolution, security hardening (path-traversal guards, decompression-bomb caps, checked integer arithmetic), unit/integration test suite, and technical documentation were written from scratch by **Mihir S. Patil**.
