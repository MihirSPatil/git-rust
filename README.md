# git-rust

A small Git client written from scratch in Rust.

I picked up this project to learn Rust by building something real from the bytes up—no `libgit2` or `gitoxide` bindings, just the standard library, `sha1`, `flate2`, and `reqwest`. Along the way, I dug into how Git actually works under the hood: content-addressable SHA-1 storage, zlib compression, binary tree serialization, `pkt-line` wire framing, packfile varint decoding, and `OFS_DELTA` / `REF_DELTA` binary patching.

## What It Supports

- `git-rust init` — creates a `.git` directory (`objects/`, `refs/heads/`, and `HEAD`)
- `git-rust hash-object [-w] <file>` — hashes a file with SHA-1 and optionally stores the zlib-compressed blob
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

I kept the codebase flat and modular so each file owns one clear part of the system:

```text
src/
├── lib.rs       # Library exports
├── main.rs      # CLI argument parsing and subcommand dispatch
├── objects.rs   # Loose object I/O, SHA-1 hashing, tree serialization, and safe checkout
└── packfile.rs  # Smart HTTP ref discovery, pkt-line framing, packfile & delta unpacking
tests/
└── integration_tests.rs
docs/
└── INTERNALS.md # Notes on Git's binary formats, packfile headers, and delta opcodes
```

## Engineering Notes

- **Fast delta lookups:** Packfiles store objects as diffs (`OFS_DELTA` and `REF_DELTA`) against earlier objects in the stream. Indexing unpacked objects in dual `HashMap`s backed by `Rc` lets deltas resolve their base objects in $O(1)$ without cloning buffers.
- **Defensive parsing:** Untrusted repositories can ship malicious tree entries or corrupt varints. Working-tree checkout blocks path traversal (`..`, `/`, `\`, `.git`), packfile sizes are capped at 512 MiB, and varint math uses checked arithmetic instead of `.unwrap()`.
- **Cross-platform:** Unix executable permissions (`100755`) are gated behind `#[cfg(unix)]` so the project builds and passes CI on Linux, macOS, and Windows.

## Verification

I verified the implementation with unit/integration tests and by running official `git` commands against repositories created by `git-rust`:

```bash
cargo test

# Cross-check with official git
cargo run --release -- clone https://github.com/codecrafters-io/git-sample-1 /tmp/test-clone
git -C /tmp/test-clone fsck --full
git -C /tmp/test-clone status
```

For a deeper breakdown of the binary wire formats and delta opcodes, see [`docs/INTERNALS.md`](docs/INTERNALS.md).

## Credit

The problem milestones and protocol specs came from CodeCrafters' [Build Your Own Git](https://codecrafters.io/challenges/git) challenge. All of the Rust implementation, architecture, tests, and notes here were written by me.
