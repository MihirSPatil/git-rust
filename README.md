# git-rust

A minimal Git client written in Rust, implementing core plumbing commands and Smart HTTP `clone` directly over SHA-1 object storage, zlib streams, `pkt-line` framing, and binary delta patching.

## Supported Commands

- `git-rust init` — creates a `.git` directory (`objects/`, `refs/heads/`, and `HEAD`)
- `git-rust hash-object [-w] <file>` — hashes a file with SHA-1 and optionally writes the compressed blob to `.git/objects/`
- `git-rust cat-file -p <sha>` — decompresses and prints a loose object
- `git-rust write-tree` — snapshots the working directory into sorted `tree` and `blob` objects
- `git-rust ls-tree [--name-only] <sha>` — parses a binary tree object and lists its entries
- `git-rust commit-tree <tree> [-p <parent>] -m <msg>` — creates a commit object
- `git-rust clone <repo-url> [target-dir]` — clones a public HTTP(S) repo over Git's Smart Protocol, unpacks deltas, and checks out the working tree

## Quickstart

```bash
# Clone a public repository
cargo run --release -- clone https://github.com/octocat/Hello-World /tmp/hello-world

# Or install the binary locally
cargo install --path .
```

## Project Layout

The codebase uses a flat, wide-and-shallow module layout so each file owns one clear boundary:

```text
src/
├── lib.rs       # Library exports (`objects` and `packfile`)
├── main.rs      # CLI argument parsing and subcommand dispatch
├── objects.rs   # Loose object I/O, SHA-1 hashing, tree serialization, safe checkout + unit tests
└── packfile.rs  # Smart HTTP ref discovery, pkt-line framing, packfile & delta unpacking + unit tests
tests/
└── integration_tests.rs
docs/
└── INTERNALS.md # Bit-level protocol & binary encoding reference
```

## Engineering Notes

- **Fast delta lookups:** Packfiles store objects as diffs (`OFS_DELTA` and `REF_DELTA`) against earlier entries in the stream. Indexing unpacked objects in dual `HashMap`s backed by `Rc` resolves delta bases in $O(1)$ without cloning multi-megabyte buffers.
- **Defensive parsing:** Working-tree checkout blocks path traversal (`..`, `/`, `\`, `.git`), unpacked object sizes are capped at 512 MiB, and varint math uses checked arithmetic instead of `.unwrap()`.
- **Cross-platform:** Unix executable permissions (`100755`) are gated behind `#[cfg(unix)]` so the project builds and passes CI on Linux, macOS, and Windows.

## Design Trade-offs & Known Limitations

- **Loose-object unpacking vs. `.idx` packfile storage:** `clone` unpacks every packfile entry directly into loose `.git/objects/` files rather than persisting the `.pack` stream alongside a `.idx` fanout index. This keeps working-tree checkout simple and verifiable with `git fsck`, at the cost of higher inode usage and disk I/O on large repositories.
- **In-memory delta base cache:** Keeping `Rc`-referenced base objects in memory guarantees $O(1)$ delta reconstruction in a single pass, but peak RAM scales with the largest delta chain in the packfile rather than spilling to disk or `mmap`.
- **Blocking HTTP transport:** Uses `reqwest::blocking` instead of an async runtime (`tokio`) to avoid async state-machine overhead on a single-stream CLI workload.
- **Scope boundaries:** Supports public HTTP/HTTPS `clone` and core plumbing (`init`, `hash-object`, `cat-file`, `write-tree`, `ls-tree`, `commit-tree`); SSH transport, shallow clones (`depth=1`), and index staging (`git add`) are intentionally out of scope.

## How to Build Something Like This Yourself

If you want to build your own Git client in Rust to explore Git internals, break it into these 7 incremental milestones:

### Milestone 1: Repository Initialization (`init`)
A Git repository is a `.git` folder with three components:
- `.git/objects/` — content-addressable object database
- `.git/refs/heads/` — branch pointers
- `.git/HEAD` — a text file containing `ref: refs/heads/main\n`

### Milestone 2: Blobs & Content-Addressable Storage (`hash-object`, `cat-file`)
Every loose object is formatted as:
```text
"<type> <byte-length>\0<raw-bytes>"
```
1. Compute the **SHA-1** hash of the combined header and body.
2. Compress the combined header and body using **zlib** (`flate2`).
3. Write the compressed bytes to `.git/objects/<first-2-hex-chars>/<remaining-38-hex-chars>`.

### Milestone 3: Binary Tree Objects (`ls-tree`, `write-tree`)
A `tree` object represents a directory as a concatenated sequence of entries (with no newline separators):
```text
"<mode> <entry-name>\0<20-byte-raw-binary-sha>"
```
- Regular files use mode `100644`, executables use `100755`, and subdirectories use `40000`.
- Entries must be sorted lexicographically by name before hashing so the resulting SHA-1 matches canonical Git.

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
1. Send `GET <repo_url>/info/refs?service=git-upload-pack`.
2. Parse `pkt-line` frames—each line starts with a **4-character hex length** (including the 4 length bytes), and `0000` is a flush packet.
3. Extract the `HEAD` commit SHA and the `symref=HEAD:refs/heads/<branch>` capability.
4. Send `POST <repo_url>/git-upload-pack` with `Content-Type: application/x-git-upload-pack-request` and body:
   ```text
   0032want <head-sha>\n
   0000
   0009done\n
   ```

### Milestone 6: Packfile Streaming & Delta Reconstruction (`OFS_DELTA` / `REF_DELTA`)
1. Read the 12-byte header: `"PACK"` magic, 4-byte version (`2` or `3`), and 4-byte big-endian object count.
2. Decode each entry's variable-length header byte (bit 7 continuation flag, bits 6..4 object type, bits 3..0 initial size bits).
3. Decompress the self-terminating zlib stream so the cursor lands directly on the next entry.
4. For `ofs_delta` (negative byte offset in the packfile) and `ref_delta` (20-byte base SHA-1), resolve the base object in `HashMap`s ($O(1)$) and run the copy/insert state machine.

### Milestone 7: Recursive Working-Tree Checkout
Read the `HEAD` commit object, extract its `tree <sha>` line, and recursively walk every `TreeEntry` into the target folder—creating subdirectories for mode `40000` and writing blob contents (plus `0o755` permissions on Unix) for files.

For a bit-level breakdown of the wire formats and delta opcodes, see [`docs/INTERNALS.md`](docs/INTERNALS.md).

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test

# Cross-check with official git
cargo run --release -- clone https://github.com/codecrafters-io/git-sample-1 /tmp/test-clone
git -C /tmp/test-clone fsck --full
git -C /tmp/test-clone status
```

## Acknowledgments & Attribution

- Built upon the [**Build Your Own Git**](https://codecrafters.io/challenges/git) challenge on [CodeCrafters](https://codecrafters.io), which provided the initial problem milestones and protocol specifications.
- Additional features built on top of the core challenge include the wide-and-shallow library/CLI architecture (`objects.rs`, `packfile.rs`), $O(1)$ `HashMap` + `Rc` delta resolution, security hardening (path-traversal guards, decompression-bomb caps, checked integer arithmetic), unit/integration test suite, cross-platform CI, and protocol documentation.
