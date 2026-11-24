# `git-rust` Internals: Objects, Wire Protocol & Packfile Encoding

A deep-dive technical reference explaining the binary formats, bit-level encodings, and data structures implemented in `git-rust`.

---

## 1. Content-Addressable Object Layout (`.git/objects`)

Every loose object (`blob`, `tree`, `commit`, `tag`) shares the exact same outer framing on disk:

```text
Uncompressed bytes:  "<type> <byte-length>\0<raw-body>"
                              │
         ┌────────────────────┴────────────────────┐
         ▼                                         ▼
   SHA-1 Hash (20 bytes)                     Zlib Stream
   -> 40-char hex string                     -> Written to:
   -> e.g. 3b18e512dba79e4c...               .git/objects/3b/18e512dba79e4c...
```

* **`blob` body:** Raw file bytes (UTF-8 text or arbitrary binary).
* **`tree` body:** Concatenated binary entries with no newline separators:
  ```text
  "<mode> <entry-name>\0<20-byte-raw-binary-sha>"
  ```
  Entries are sorted lexicographically by `entry-name` prior to hashing so the resulting tree SHA-1 matches canonical Git deterministically.
* **`commit` body:** Plain UTF-8 key-value headers (`tree`, optional `parent`, `author`, `committer`), a blank line, and the commit message.

---

## 2. Smart HTTP Discovery & `pkt-line` Framing

Git's Smart HTTP protocol frames variable-length records using a **4-character lowercase hexadecimal length prefix** (where the length includes the 4 prefix bytes themselves):

```text
"0032want 3b0466d22854e57bf9ad3ccf82008a2d3f199550\n"
 └──┬┘└─────────────────────┬───────────────────────┘
  0x0032 = 50 bytes       46 payload bytes (4 + 46 = 50)
```

* **`0000` (Flush Packet):** Signals the end of a section (such as the end of capability advertisements or the end of `want` lines).
* **Ref Discovery (`GET /info/refs?service=git-upload-pack`):** Advertises `HEAD` alongside NUL-separated capabilities, including `symref=HEAD:refs/heads/<default-branch>`.
* **Pack Negotiation (`POST /git-upload-pack`):** Sends `want <head-sha>\n`, `0000`, and `done\n` with `Content-Type: application/x-git-upload-pack-request`.

---

## 3. Packfile Binary Stream & Bit-Level Decoding

The server responds with a binary `PACK` stream:

```text
[ "PACK" (4 B) ] [ Version: 2 or 3 (4 B BE) ] [ Object Count (4 B BE) ] [ Entry 1 ] [ Entry 2 ] ...
```

### Variable-Length Object Header Byte
Each packfile entry begins with a 1-byte header packed into three bit fields:

```text
Bit position:   7     6   5   4     3   2   1   0
Place value:   128    64  32  16    8   4   2   1
              [MSB]  [---Type---]  [----Size-----]
```

* **`byte >= 128` (Bit 7 — Continuation Flag):** If `1`, additional little-endian 7-bit size bytes follow (shifted left by `4`, `11`, `18`, ... bits).
* **`(byte % 128) / 16` (Bits 6..4 — Object Type):** `% 128` clears the MSB; integer division by `16` shifts out the lower 4 size bits, leaving a type code in `0..=7`:
  * `1` = `commit`, `2` = `tree`, `3` = `blob`, `4` = `tag`
  * `6` = `ofs_delta` (base object referenced by negative byte offset within the same packfile)
  * `7` = `ref_delta` (base object referenced by its 20-byte SHA-1 hash)
* **`byte % 16` (Bits 3..0 — Initial Size):** Keeps the bottom 4 bits (`0..=15`) of the uncompressed object size.

### Self-Terminating Zlib Streams
Immediately after the header (and optional delta base reference), the compressed payload is stored as a standard zlib stream. Because zlib embeds its own end-of-stream marker and Adler-32 checksum, `ZlibDecoder` stops reading at the exact byte boundary where the next packfile entry starts.

---

## 4. Delta Reconstruction (`OFS_DELTA` & `REF_DELTA`)

### `OFS_DELTA` Offset Encoding
`OFS_DELTA` stores a big-endian base-128 negative distance back to its base object's `entry_offset`. To avoid redundant multi-byte encodings, each continuation step adds `1` before shifting by `128`:

```text
distance = ((distance + 1) * 128) + (next_byte % 128)
target_offset = entry_offset - distance
```

### Delta Patch Instruction Stream
Once decompressed, a delta payload begins with two little-endian base-128 varints (`expected_base_size` and `target_size`), followed by a sequence of opcodes:

* **Copy Opcode (`opcode >= 128`):**
  * Bits `0..=3` indicate which of the 4 little-endian `copy_offset` bytes follow.
  * Bits `4..=6` indicate which of the 3 little-endian `copy_length` bytes follow (if `0`, length defaults to `65,536` bytes).
  * Copies `base[copy_offset .. copy_offset + copy_length]` into the output buffer.
* **Insert Opcode (`1..=127`):**
  * Copies the next `opcode` literal bytes directly from the delta stream into the output buffer.

### $O(1)$ Base Resolution with `HashMap` + `Rc`
Because Git servers emit base objects before any deltas that depend on them, `unpack_packfile_into_repo` indexes every unpacked object in two `HashMap`s (`unpacked_by_offset` and `unpacked_by_sha`) pointing to a shared `Rc<(GitObjectKind, Vec<u8>)>`. Both `OFS_DELTA` and `REF_DELTA` resolve their base objects in **$O(1)$** time with zero buffer cloning.
