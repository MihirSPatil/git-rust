# AGENTS.md — Repository & Architecture Guide

## Architecture: Wide & Shallow Modules
This repository follows a flat, wide-and-shallow module layout (inspired by John Ousterhout and Matt Pocock) so the entire codebase can be navigated without deep directory trees:

```text
src/
├── lib.rs       # Public crate exports (`objects` and `packfile`)
├── main.rs      # Thin CLI argument parser and subcommand dispatch
├── objects.rs   # Deep module: .git initialization, SHA-1 loose objects, tree serialization, safe checkout + unit tests
└── packfile.rs  # Deep module: Smart HTTP discovery, pkt-line framing, packfile & delta unpacking + unit tests
tests/
└── integration_tests.rs  # End-to-end public-API round-trip & security tests
docs/
└── INTERNALS.md # Bit-level protocol & binary format reference
```

### Design Rules
1. **Keep modules flat and cohesive:** Do not split `src/objects.rs` or `src/packfile.rs` into nested sub-directories or tiny micro-files. Keep domain logic, private helpers, and `#[cfg(test)]` unit tests co-located in the same file.
2. **Maintain narrow public interfaces:** Only expose high-level domain entry points (`pub fn`) from `objects.rs` and `packfile.rs`. Keep low-level bit/varint/path helpers private (`fn`).
3. **Zero panics in library or CLI paths:** Never introduce `.unwrap()` or `.expect()` in `src/`. Propagate all errors with `anyhow::Result` and `.context(...)`.
4. **Security & bounds invariants:**
   - All tree entry names checked out to disk must pass `is_safe_entry_name` (blocking `..`, `/`, `\`, NUL bytes, and `.git`).
   - All varint shifts/additions and delta slice bounds must use checked arithmetic (`checked_add`, `checked_shl`, `checked_sub`, `.get(...)`).
   - Unpacked object sizes must stay below `MAX_UNPACKED_OBJECT_BYTES` (512 MiB).
5. **Cross-platform compatibility:** Gate Unix-specific file permissions (`0o755` / `PermissionsExt`) behind `#[cfg(unix)]`.

## Verification Commands
Run all three checks before completing any change:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```
