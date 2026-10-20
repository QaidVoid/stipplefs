# The stipplefs image format, version 1.3

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
| extents, attributes, extended attributes, block   |
| index, dictionaries)                              |
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
| 10 | 2 | `version_minor` | `3` for this document |
| 12 | 2 | `flags` | superblock flags, see below |
| 14 | 2 | `codec_mask` | bitmask of the block codecs used in the image |
| 16 | 8 | `image_length` | total size of the image in bytes |
| 24 | 8 | `stream_length` | total length of the logical stream |
| 32 | 4 | `block_size` | decoded size of a full block, a power of two |
| 36 | 4 | `inode_count` | number of inodes |
| 40 | 4 | `extent_count` | number of entries in the extent table |
| 44 | 4 | `block_count` | number of entries in the block index |
| 48 | 4 | `root_inode` | inode number of the root directory |
| 52 | 4 | reserved | zero |
| 56 | 128 | `sections` | eight `(offset, length)` pairs, see below |
| 184 | 8 | `hot_length` | bytes at the front of the stream a hot list fills, see below |
| 192 | 3896 | reserved | zero |
| 4088 | 8 | `checksum` | `xxh3-64` over bytes `[0, 4088)` |

### Superblock flags

| Bit | Name | Meaning |
|---|---|---|
| `0x0001` | `FLAG_COMPRESSED_METADATA` | present compressible metadata sections are stored as compressed frames |

Every other bit is reserved and SHALL be zero. A reader SHALL reject an image
whose `flags` contains a bit it does not implement. A reader SHALL reject
`FLAG_COMPRESSED_METADATA` on an image whose `version_minor` is below 2.

### Section table

Eight `(offset [8], length [8])` pairs, in this fixed order:

| Index | Section |
|---|---|
| 0 | inode table |
| 1 | directory entries |
| 2 | name table |
| 3 | extent table |
| 4 | block index |
| 5 | dictionaries |
| 6 | attribute table |
| 7 | extended attributes |

A section with `length == 0` is absent and its `offset` SHALL be zero. The
indices are the order of the pairs in the superblock, not necessarily the order
the sections appear in the image. See Canonical encoding.

A reader SHALL reject the image unless all of the following hold:

1. `magic` matches.
2. `version_major == 1`, and `version_minor` is at least 1. A greater
   `version_minor` is accepted. The inode table's layout changed at minor
   version 1, and no reader can interpret both, so a reader implementing this
   document rejects minor version 0.
3. `checksum` matches the computed digest.
4. `image_length` is a non-zero multiple of 4096 and does not exceed the range
   the caller supplied.
5. `block_size` is a power of two, at least 4096 and at most 2^31.
6. Every present section lies entirely within `[4096, image_length)`.
7. No two present sections overlap.
8. Every bit set in `codec_mask` names a codec this document defines and the
   reader implements.
9. No bit is set in `flags` outside the flags this document defines.
10. `FLAG_COMPRESSED_METADATA` is clear when `version_minor` is below 2.
11. `hot_length` does not exceed `stream_length`.

Rule 8 is what makes an unsupported codec fail at open rather than at the first
affected read.

### Hot region

A writer may be told which files an application reads while it starts, and
place them at the front of the stream in the order they are read. `hot_length`
records how many stream bytes those files fill, so a reader can begin decoding
that region before anything asks for it. It is zero when the writer was told of
no such files. The field is defined from minor version 3; in an earlier image
its bytes are reserved and a reader takes it as zero. It is a hint and changes
nothing a read returns.

## Codecs

| Id | Bit | Name | Meaning |
|---|---|---|---|
| 0 | `0x0001` | `stored` | block bytes are the decoded bytes |
| 1 | `0x0002` | `zstd` | Zstandard frame, no dictionary |
| 2 | `0x0004` | `zstd-dict` | Zstandard frame using the image's dictionary |
| 3 | `0x0008` | `zstd-bcj` | Zstandard frame, no dictionary, over BCJ-filtered x86 bytes |
| 4 | `0x0010` | `zstd-dict-bcj` | Zstandard frame using the image's dictionary, over BCJ-filtered x86 bytes |

Codecs 3 and 4, and `FLAG_COMPRESSED_METADATA`, are defined from minor
version 2.

`codec_mask` is a bitmask of the block codecs used in the image. Every block's
`codec` bit SHALL be set in `codec_mask`. A reader SHALL reject a mask that
contains a bit the format does not define, or a bit for a codec the reader does
not implement. A mask that contains a supported codec no block uses is valid;
the writer always sets the `stored` bit. Metadata compression is signalled by
`FLAG_COMPRESSED_METADATA`, not by `codec_mask`.

Codecs 3 and 4 apply the BCJ transform described below. A reader that does not
implement `zstd` can read an image whose `codec_mask` is `0x0001` when
`FLAG_COMPRESSED_METADATA` is clear. When the flag is set and a compressible
section is present, the reader cannot expand it and SHALL refuse the image.

## BCJ transform

Codecs 3 and 4 apply the x86 BCJ filter from xz's `simple/x86.c` (0BSD; Igor
Pavlov and Lasse Collin) to the block's decoded bytes. On encode, the block's
bytes are filtered before the Zstandard frame is produced. On decode, the frame
is expanded and the filter's decoder direction is applied to the result.

The encoder direction converts relative call and jump displacements to absolute
targets; the decoder direction converts absolute targets back to relative
displacements. The transform is length-preserving.

Positions are block-relative and start at zero for every block. Filter state
does not carry between blocks, so a block decodes without knowing where it sits
in the stream. A candidate is considered only when its opcode and four-byte
operand lie within the block, so the filter never reads or writes past the end
of the block. The filter has no separate tail: the last four bytes are
rewritten when they are the operand of a candidate whose opcode is at
`len - 5`.

The transform is the following procedure. Arithmetic on `u32` values wraps
modulo 2^32. `buf` is the block's bytes and `len` is its length. The encoder
direction uses `+` where the procedure says so; the decoder direction uses `-`.

```text
if len < 5: return
prev_mask = 0
prev_pos = 2^32 - 5
limit = len - 5
pos = 0
while pos <= limit:
    if buf[pos] is not 0xE8 and not 0xE9:
        pos += 1
        continue
    offset = pos - prev_pos
    prev_pos = pos
    if offset > 5:
        prev_mask = 0
    else:
        repeat offset times:
            prev_mask = (prev_mask & 0x77) << 1
    tail = buf[pos + 4]
    if tail is 0x00 or 0xFF, and (prev_mask >> 1) <= 4, and (prev_mask >> 1) != 3:
        src = (tail << 24) | (buf[pos + 3] << 16) | (buf[pos + 2] << 8) | buf[pos + 1]
        loop:
            dest = src + (pos + 5)
            if prev_mask == 0: break
            i = [0, 1, 2, 2, 3][prev_mask >> 1]
            b = (dest >> (24 - 8 * i)) & 0xFF
            if b is not 0x00 and not 0xFF: break
            src = dest XOR ((1 << (32 - 8 * i)) - 1)
        buf[pos + 4] = 0xFF if (dest >> 24) & 1 else 0x00
        buf[pos + 3] = (dest >> 16) & 0xFF
        buf[pos + 2] = (dest >> 8) & 0xFF
        buf[pos + 1] = dest & 0xFF
        pos += 5
        prev_mask = 0
    else:
        pos += 1
        prev_mask |= 1
        if tail is 0x00 or 0xFF:
            prev_mask |= 0x10
```

For the decoder direction, replace `dest = src + (pos + 5)` with
`dest = src - (pos + 5)`.

## Compressed metadata

When `FLAG_COMPRESSED_METADATA` is set, every present section for which
`SectionKind::is_compressible()` holds is stored as a compressed frame. Those
sections are the inode table, directory entries, name table, extent table,
attribute table, and extended attributes. The block index and the dictionaries
section are always stored raw.

A compressed section has this layout:

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | `decoded_length`, the section's decoded size |
| 8 | rest | one Zstandard frame with no dictionary, up to the digest |
| ... | 8 | `xxh3-64` digest of the stored bytes |

The section's `length` includes the digest. The digest covers the stored bytes,
that is the eight-byte length prefix and the frame, not the decoded content. The
prefix records the section's decoded length; the reader uses it to bound the
decompression. For a section stored raw, the stored bytes are the section's
content, so the digest covers the raw content.

The block index stays raw because its entries record the offsets of the data
blocks that follow it. Compressing it would make the layout depend on the
compressed size of a section whose content depends on the layout. The
dictionaries section stays raw because a dictionary is close to incompressible
itself.

A reader SHALL reject `FLAG_COMPRESSED_METADATA` on an image whose
`version_minor` is below 2. A reader without zstd support cannot expand a
compressed section and SHALL fail rather than return the stored bytes as
content.

## Block index

`block_count` entries of 20 bytes each, in ascending order of `stream_offset`.

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | `image_offset`, where the stored bytes begin |
| 8 | 4 | `stored_length`, bytes as stored |
| 12 | 4 | `decoded_length`, bytes after decoding |
| 16 | 1 | `codec` |
| 17 | 3 | reserved |

Block `i` covers logical stream range `[i * block_size, i * block_size +
decoded_length)`. The format places no useful ceiling on `block_size`: large
blocks compress better and cost more to read, and choosing where to sit on that
curve is the writer's business, not the format's. Because blocks are laid out in stream order at a fixed
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

Inodes are stored as parallel arrays rather than as records, one array per
field, each entry a little-endian unsigned integer of a width the image
chooses. A tree's largest file, its longest stream and its number of distinct
attribute tuples all vary by orders of magnitude between images, and a fixed
record has to be wide enough for the worst of them on every inode.

The section begins with a sixteen byte header:

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | `width_size`, 1 to 8 |
| 1 | 1 | `width_payload`, 1 to 8 |
| 2 | 1 | `width_attrs`, 1 to 4 |
| 3 | 1 | `width_nlink`, 1 to 4 |
| 4 | 1 | `width_mtime`, 0 to 8, zero when no time is recorded |
| 5 | 1 | `width_xattr`, 0 to 4, zero when no inode has attributes |
| 6 | 1 | `width_extents`, 1 to 4 |
| 7 | 1 | reserved |
| 8 | 8 | `mtime_base`, seconds the recorded offsets are measured from |

The arrays follow the header back to back, each holding `inode_count` entries,
in this order: `size`, `payload`, `attrs`, `nlink`, `mtime`, `xattr`,
`extents`. An array whose width is zero is absent entirely and occupies no
space. Inode `i`'s value in an array beginning at `base` with width `w` is the
`w` bytes at `base + i * w`, read little-endian, so lookup remains a single
indexed read.

A reader SHALL reject the section unless every width lies in the range its row
gives, and unless the arrays together with the header exactly fill the section
apart from its trailing digest.

### What each array holds

- **`size`**: file size in bytes for a regular file, entry count for a
  directory, target length in bytes for a symbolic link, and zero otherwise.
- **`payload`**: for a regular file with one extent, the stream offset of that
  extent; with more than one, the index of its first entry in the extent table.
  For a directory, the index of its first entry in the directory entry table.
  For a symbolic link, the byte offset of its target in the name table. For a
  character or block device, the device number, with the major number in the
  upper thirty-two bits and the minor in the lower. Zero otherwise.
- **`attrs`**: an index into the attribute table, which carries the mode and so
  determines the entry's kind.
- **`nlink`**: the number of names referring to this inode, at least one.
- **`mtime`**: seconds after `mtime_base`. Absent when `width_mtime` is zero,
  in which case a reader reports no time.
- **`xattr`**: zero when the inode has no extended attributes, otherwise one
  more than an index into the extended attribute table. Absent when
  `width_xattr` is zero.
- **`extents`**: the number of extents, zero for anything that is not a regular
  file and for an empty one.

## Attribute table

Entries of 12 bytes, holding the values that repeat across a tree.

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | `mode`, POSIX type and permission bits |
| 4 | 4 | `uid` |
| 8 | 4 | `gid` |

Entries SHALL be sorted ascending by `mode`, then `uid`, then `gid`, and SHALL
be distinct. Sorting makes the order a function of the content rather than of
the order the writer happened to encounter values.

An inode's kind comes from the type bits of its entry's `mode`. The format
defines seven: regular file, directory, symbolic link, named pipe, socket,
character device and block device. A reader SHALL reject an inode whose mode
names none of them, and SHALL reject an `attrs` index at or beyond the number
of entries.

## Extended attributes

Absent unless some inode has attributes. The section holds, in order:

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | `set_count`, the number of distinct attribute sets |
| 4 | 4 | `pair_count`, the number of name and value pairs across all sets |
| 8 | 4 x (`set_count` + 1) | `set_starts`, where each set's pairs begin, ascending, the last being `pair_count` |
| ... | 12 x `pair_count` | pairs, see below |
| ... | rest | the byte pool names and values point into |

Each pair is 12 bytes:

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | `name_offset` into the pool |
| 4 | 2 | `name_length` |
| 6 | 4 | `value_offset` into the pool |
| 10 | 2 | `value_length` |

Set `k` owns the pairs from `set_starts[k]` up to `set_starts[k + 1]`. A name
that repeats across sets SHALL occupy the pool once, so a tree where every file
carries the same attribute name pays for that name a single time. Pairs within
a set SHALL be sorted ascending by name bytes.

A reader SHALL reject the section unless `set_starts` increases, ends at
`pair_count`, and every offset and length lies inside the pool.

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

## Dictionaries

The dictionaries section holds a single Zstandard dictionary, and its content
is exactly that dictionary's bytes. A block whose codec is `zstd-dict` or
`zstd-dict-bcj` is decoded with it. For `zstd-dict-bcj`, the dictionary applies
to the BCJ-filtered bytes.

An image whose `codec_mask` sets either dictionary bit SHALL have a present
dictionaries section, and an image whose `codec_mask` sets neither SHALL have
it absent. A reader SHALL reject an image whose `codec_mask` sets either
dictionary bit without a dictionaries section.

One dictionary per image is deliberate. Blocks are compressed independently and
seeded from the same dictionary, which is what lets a small block reach a ratio
close to a much larger one without enlarging the decode unit.

## Integrity

| Covered | Digest location |
|---|---|
| Superblock bytes `[0, 4088)` | superblock offset 4088 |
| Each metadata section's stored bytes | 8-byte `xxh3-64` immediately after them, inside the section's `length` |
| Each block's stored bytes | 8-byte `xxh3-64` immediately after them, outside `stored_length` |

A reader SHALL verify the superblock digest before trusting any other field. It
SHALL verify a metadata section's digest before first use of that section, and
a block's digest before returning any byte decoded from it.

## Canonical encoding

A given logical image has exactly one canonical encoding. A writer SHALL follow
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
6. Sections appear in the order inodes, dirents, names, extents, attributes,
   extended attributes, blocks, dictionaries (section kind indices 0, 1, 2, 3,
   6, 7, 4, 5), each starting at the next multiple of 4096.
7. All padding is zero.
8. Reserved fields are zero.
9. A block is stored with codec `stored` when no available codec produces a
   strictly smaller result. When a BCJ codec is permitted, the writer also
   compresses the block with the BCJ filter applied and keeps the smallest
   result. Among codecs producing the same size, the lowest codec id wins.
10. Attribute table entries are sorted ascending by mode, then user, then
    group.
11. Each inode array's width is the smallest that represents every value it
    must hold, so the widths are a function of the content.
12. Extended attribute sets appear in order of first reference by rule 2, pairs
    within a set are sorted by name bytes, and a repeated name or value
    occupies the pool once.
13. `mtime_base` is the smallest modification time the image records, or zero
    when it records none.
14. The superblock sets `FLAG_COMPRESSED_METADATA`.
15. `codec_mask` contains the `stored` bit and the bit of every codec a block
    uses.
16. `hot_length` is zero.

An image with `FLAG_COMPRESSED_METADATA` clear is valid but non-canonical. The
canonical encoding uses walk order for the stream; a writer that reorders it,
for example with `mkstipple --order ext` or `mkstipple --hot-list`, produces a
valid but non-canonical image.

Rule 9 exists because a size tie between codecs would otherwise leave the
choice to the writer's iteration order, which is not determined by content.
