# Architecture

<p class="lead">
How an image is laid out, how a read finds its bytes, and how a build and a mount do their work. The <a href="/format">specification</a> defines every byte. This page explains why the pieces fit together the way they do.
</p>

## One stream, cut into blocks

At the heart of an image is the **logical stream**: the deduplicated contents of every file, laid end to end. A file does not own a range of the image. It owns a list of **extents**, each naming a range of the stream. A file whose content appears once is usually a single extent. A file that shares runs with others has several, some pointing at bytes another file placed first.

The stream is cut into **blocks** of one fixed size, and each block is compressed on its own:

```text
stream  | a.so ................| b.so, its new bytes only .......| c.py ...|
blocks  |-------- block 0 ---------|-------- block 1 ---------|- block 2 --|
image   |superblock|metadata| z0 | z1 | z2 |
```

Block boundaries fall wherever the block size puts them, regardless of where files begin and end. A small file usually sits inside one block, and a large one spans several.

Because every block but the last has the same decoded size, the block holding stream offset `x` is `x / block_size`. Finding it is a division, not a search, and the block index is a flat table read directly by position.

Every offset in the image is relative to the image's first byte, which is what lets an image live at any position inside a larger file.

## Metadata

Metadata sits between the superblock and the blocks, in sections that each start on a 4096-byte boundary and end with a digest:

| Section | Holds |
|---|---|
| Inodes | Parallel arrays, one per field, of size, payload, attributes, link count, time, extended attributes and extent count. |
| Directory entries | Each directory's children as one contiguous run, sorted by name. |
| Names | Every distinct name, stored once. |
| Extents | Each file's extents as one contiguous run, in file order. |
| Attributes | Each distinct combination of mode, user and group, stored once. |
| Extended attributes | Each distinct attribute set, with names and values pooled. |
| Block index | Where each block is stored, its sizes and its codec. |
| Dictionary | A Zstandard dictionary, when the image uses one. |

The inode arrays are the reason metadata stays small. Each array's entries are exactly as wide as its largest value needs, chosen per image. A tree whose files are all under 16 MiB stores sizes in three bytes. Most trees have a handful of distinct permission and ownership combinations, so each inode stores a one-byte index into the attribute table. The Python standard library spends 12.7 bytes of compressed metadata per inode.

Lookups never scan. A child is found by binary search over its directory's sorted entries. An offset within a file is found by binary search over its extents. An inode's fields are indexed reads at a computed position in each array.

## A read, start to finish

Reading 4 KiB at offset `o` of `lib/a.so`:

1. **Resolve.** Binary search the root's entries for `lib`, then `lib`'s entries for `a.so`, giving an inode number.
2. **Locate.** Binary search the file's extents for the one holding `o`, and turn `o` into a stream offset.
3. **Fetch.** Divide by the block size to get the block. Ask the cache for it.
4. **Decode.** On a miss, read the stored bytes, verify their digest, and decompress. When the codec is a filtered one, undo the branch filter. Concurrent readers of the same block wait for this one decode.
5. **Copy.** Copy the requested range out. A read that crosses an extent or block boundary repeats steps 3 to 5 for the rest.

Every metadata section was verified once when the reader was created, so steps 1 and 2 touch no digests.

## Codecs

Each block records its own codec:

| Codec | Encoding |
|---|---|
| `stored` | The bytes as they are. |
| `zstd` | A Zstandard frame. |
| `zstd-dict` | A Zstandard frame against the image's dictionary. |
| `zstd-bcj` | A Zstandard frame of the bytes after the x86 branch filter. |
| `zstd-dict-bcj` | Both of the above. |

The writer tries every permitted codec on every block and keeps the smallest, with ties going to the lowest codec number, so the choice depends only on the content. The superblock records which codecs appear, so a reader missing one refuses the image at open rather than failing on some later read.

The branch filter is the one xz uses for x86. Calls and jumps in machine code store their targets relative to the instruction, so calls to one function from many places all look different. The filter rewrites them as absolute targets, which repeat, and the compressor finds the repeats. Its state starts afresh in every block, so any block decodes without its neighbours.

The compression window is set to cover the whole block. Zstandard otherwise caps its window at 8 MiB below level 20, and in a 32 MiB block anything further back than that would never be matched.

## Building

```text
walk ──► place ──► compress ──► write
 tree      stream     blocks      image
 metadata  extents    in parallel
```

**Walk** reads the tree in sorted order and records structure only. File contents stay on disk until they are placed, so memory does not grow with the size of the tree.

**Place** reads files in the chosen order and cuts each into chunks with FastCDC, whose boundaries depend on the content around them. Each chunk is hashed with 128-bit XXH3. A chunk seen before becomes an extent pointing at the earlier copy, and a new one is appended to the stream. The stream goes to a scratch file, not memory.

**Compress** hands blocks to worker threads, each holding one block at a time. Results go to the scratch file in block order. A memory ceiling lowers how many blocks are in flight.

**Write** encodes the metadata, compresses each section, lays everything out on 4096-byte boundaries, computes the digests and streams the image out.

The output depends only on the tree and the options. Thread counts, memory limits and the order the operating system lists directory entries in have no effect. The test suite builds the same tree in separate processes and compares the bytes, because two builds in one process share hash seeds and would agree even when the writer was not deterministic.

## Mounting and warming

The mount is a FUSE filesystem in front of a reader. Its distinctive part is **warming**, which exists because a large block is far more expensive to decode than the read that asks for it.

When the mount starts it builds a table of **pieces**: for every block, which part of which file it holds. A warming worker takes a block from a queue, decodes it, and for each piece stores the file's bytes into the kernel's page cache through FUSE's store notification. After that the kernel answers reads of those bytes itself, and the decoded block is dropped.

Two details make that work:

- **The kernel only accepts stores for inodes it knows.** Before storing a block, a worker looks up each file in it through the mountpoint, so the kernel holds the inode.
- **The kernel marks cached data valid one page at a time**, and only when a store covers the whole page from its start. A file's bytes rarely begin or end on page boundaries within a block. Each piece's interior is stored directly. The partial pages at its ends are held aside until the neighbouring piece in the next block arrives, and then stored whole.

The queue has two lanes. Blocks something is waiting on, because a read touched them or a file in them was opened, go in the urgent lane. Blocks warmed in case they are wanted, such as the hot region at mount time and the blocks after each read, go in the later lane. Workers always drain the urgent lane first.

## Integrity

| What | Covered by |
|---|---|
| Superblock | An XXH3-64 digest in its last eight bytes, checked before any field is trusted. |
| Each metadata section | A digest at its end, checked once when a reader is created. |
| Each block | A digest after its stored bytes, checked before anything decoded from it is returned. |

Digests cover the stored bytes, so damage is caught before decompression ever sees it.

## Testing

The test suite includes a second reader written from the specification alone, which parses images the crate writes and must agree with the crate's own reader. Others build the same tree in separate processes and compare the bytes, round-trip every kind of entry through build, mount and extract, keep the writer's memory flat as trees grow, and check that warming stores exactly the bytes each file holds. A feature matrix builds every combination of features, so a reader-only or writer-only build cannot silently break.
