# The stipplefs image format, version 1.0

This document defines the on-disk format completely enough to write an
independent reader. It is normative: where it says SHALL, a conforming reader
rejects an image that violates the rule.

## Conventions

- All integers are **unsigned little-endian**, of the width given.
- All offsets are **relative to the first byte of the image**, never to the
  start of a containing file. An image may therefore be embedded at any offset
  inside a larger file.
- `xxh3-64` means the 64-bit XXH3 digest with seed 0.
- Reserved fields SHALL be written as zero and SHALL be ignored when read.
- Sizes in brackets, such as `[8]`, are byte counts.

## Layout

```
+--------------------------------------------------+ offset 0
| Superblock                                 [4096] |
+--------------------------------------------------+
| Metadata sections (inodes, dirents, names,        |
| extents, block index, dictionaries)               |
+--------------------------------------------------+
| Data blocks                                       |
+--------------------------------------------------+ offset image_length
```

Every section begins at a multiple of 4096 from the image start. The final
section is padded with zero bytes to a multiple of 4096, so `image_length` is
always a multiple of 4096. Alignment exists so that a future kernel driver can
issue aligned reads.

## Superblock

Occupies exactly 4096 bytes at image offset 0.

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 8 | `magic` | ASCII `STIPPLE\0`, bytes `53 54 49 50 50 4C 45 00` |
| 8 | 2 | `version_major` | `1` for this document |
| 10 | 2 | `version_minor` | `0` for this document |
| 12 | 2 | `flags` | reserved, zero in version 1.0 |
| 14 | 2 | `codec_mask` | bitmask of every codec used anywhere in the image |
| 16 | 8 | `image_length` | total size of the image in bytes |
| 24 | 8 | `stream_length` | total length of the logical stream |
| 32 | 4 | `block_size` | decoded size of a full block, a power of two |
| 36 | 4 | `inode_count` | number of inodes |
| 40 | 4 | `extent_count` | number of entries in the extent table |
| 44 | 4 | `block_count` | number of entries in the block index |
| 48 | 4 | `root_inode` | inode number of the root directory |
| 52 | 4 | reserved | zero |
| 56 | 8x2 x 6 | `sections` | six `(offset, length)` pairs, see below |
| 152 | 3936 | reserved | zero |
| 4088 | 8 | `checksum` | `xxh3-64` over bytes `[0, 4088)` |

### Section table

Six `(offset [8], length [8])` pairs, in this fixed order:

| Index | Section |
|---|---|
| 0 | inode table |
| 1 | directory entries |
| 2 | name table |
| 3 | extent table |
| 4 | block index |
| 5 | dictionaries |

A section with `length == 0` is absent and its `offset` SHALL be zero.

A reader SHALL reject the image unless all of the following hold:

1. `magic` matches.
2. `version_major == 1`. A greater `version_minor` is accepted.
3. `checksum` matches the computed digest.
4. `image_length` is a multiple of 4096 and does not exceed the range the
   caller supplied.
5. `block_size` is a power of two, at least 4096 and at most 2^23. The
   upper bound is set by `decoded_length` being three bytes.
6. Every present section lies entirely within `[4096, image_length)`.
7. No two present sections overlap.
8. Every bit set in `codec_mask` names a codec the reader implements.

Rule 8 is what makes an unsupported codec fail at open rather than at the first
affected read.

## Codecs

| Id | Bit | Name | Meaning |
|---|---|---|---|
| 0 | `0x0001` | `stored` | block bytes are the decoded bytes |
| 1 | `0x0002` | `zstd` | Zstandard frame, no dictionary |
| 2 | `0x0004` | `zstd-dict` | Zstandard frame using the image's dictionary |

`codec_mask` SHALL have exactly the bits of the codecs actually used. A reader
that does not implement `zstd` may still open an image whose `codec_mask` is
`0x0001`.

## Block index

`block_count` entries of 16 bytes each, in ascending order of `stream_offset`.

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | `image_offset`, where the stored bytes begin |
| 8 | 4 | `stored_length`, bytes as stored |
| 12 | 3 | `decoded_length`, bytes after decoding |
| 15 | 1 | `codec` |

Block `i` covers logical stream range `[i * block_size, i * block_size +
decoded_length)`. Because blocks are laid out in stream order at a fixed
`block_size`, the block holding a stream offset is `offset / block_size`, an
index rather than a search. Only the final block may have `decoded_length`
less than `block_size`.

A reader SHALL reject the image unless, for every block: the stored bytes lie
within `[4096, image_length)`; `decoded_length` is at most `block_size`; and
`codec`'s bit is set in `codec_mask`. When decoding produces a length other
than `decoded_length`, the read SHALL fail and return nothing to the caller.

Each block's stored bytes are followed by an 8-byte `xxh3-64` digest of those
stored bytes. `stored_length` excludes the digest.

## Extent table

`extent_count` entries of 24 bytes each. A file's extents occupy a contiguous
run of this table and appear in ascending order of `file_offset`.

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | `stream_offset` |
| 8 | 8 | `file_offset` |
| 16 | 4 | `length` |
| 20 | 4 | reserved |

For a file with extents `e[0..n]`, a reader SHALL reject the image unless
`e[0].file_offset == 0`, each `e[k].file_offset == e[k-1].file_offset +
e[k-1].length`, and the sum of lengths equals the inode's `size`. Together
these forbid gaps, overlaps, and disagreement with the recorded size.

Resolving a file offset is a binary search over that run on `file_offset`.

## Inode table

`inode_count` entries of 40 bytes each, indexed by inode number starting at 0.

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | `size` for a file, entry count for a directory, target length for a link |
| 8 | 8 | `payload`, see below |
| 16 | 4 | `extent_count`, zero for non-files |
| 20 | 4 | `mode`, POSIX type and permission bits |
| 24 | 4 | `uid` |
| 28 | 4 | `gid` |
| 32 | 4 | `nlink` |
| 36 | 4 | reserved |

`payload` is interpreted by file type:

- **Regular file**, `extent_count == 1`: `payload` is the `stream_offset` of
  the file's single extent, and the extent table is not consulted.
- **Regular file**, `extent_count > 1`: `payload` is the index of the file's
  first entry in the extent table.
- **Directory**: `payload` is the index of its first entry in the directory
  entry table.
- **Symbolic link**: `payload` is a byte offset into the name table where the
  target begins.

A regular file with `extent_count == 0` SHALL have `size == 0`.

> The single-extent case is inlined because measurement across three corpora
> found 83% to 99.6% of files have exactly one extent. Its length is already
> `size` and its `file_offset` is necessarily zero, so only the stream offset
> is needed.

## Directory entries and names

A directory's entries occupy a contiguous run of the directory entry table,
sorted by name, compared as byte strings.

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | `inode` |
| 4 | 4 | `name_offset`, byte offset into the name table |
| 8 | 2 | `name_length` |
| 10 | 2 | reserved |

Sorting is by unsigned byte value, so lookup is a binary search. Names are raw
bytes and SHALL NOT contain `0x00` or `/`. The name table is a concatenation of
name bytes with no separators and no terminators.

## Integrity

| Covered | Digest location |
|---|---|
| Superblock bytes `[0, 4088)` | superblock offset 4088 |
| Each metadata section | 8-byte `xxh3-64` immediately after the section's bytes, inside its `length` |
| Each block's stored bytes | 8-byte `xxh3-64` immediately after them, outside `stored_length` |

A reader SHALL verify the superblock digest before trusting any other field. It
SHALL verify a metadata section's digest before first use of that section, and
a block's digest before returning any byte decoded from it.

## Canonical encoding

A given logical image has exactly one valid encoding. A writer SHALL follow
every rule below, and two conforming writers therefore produce byte-identical
output from identical input.

1. Directory entries are sorted by name as unsigned bytes.
2. Inode numbers are assigned by a pre-order walk of the tree from the root,
   visiting each directory's entries in the sorted order of rule 1.
3. Names appear in the name table in order of first reference by rule 2, and a
   repeated name is stored once and referenced again.
4. Blocks appear in ascending stream order, and the stream is filled in the
   order content is first referenced by rule 2.
5. A file's extents appear in ascending `file_offset`.
6. Sections appear in the order given by the section table, each starting at
   the next multiple of 4096.
7. All padding is zero.
8. Reserved fields are zero.
9. A block is stored with codec `stored` when no available codec produces a
   strictly smaller result. Among codecs producing the same size, the lowest
   codec id wins.

Rule 9 exists because a size tie between codecs would otherwise leave the
choice to the writer's iteration order, which is not determined by content.
