# Building images

<p class="lead">
<code>mkstipple</code> turns a directory into an image. The defaults suit an application tree of a few hundred megabytes or more. This page explains what each setting does, so you know when to change one.
</p>

```sh
mkstipple <SOURCE> <OUTPUT> [OPTIONS]
```

Every option is listed in the [mkstipple reference](/reference/mkstipple).

## What a build does

A build runs in four stages, and the progress line names the one in progress.

1. **Walk.** The source tree is read in sorted order, and the structure and metadata of every entry are recorded. File contents stay on disk for now. Hard links in the source are recognised by inode and stay hard links in the image.
2. **Place.** Files are read in the chosen order, cut into chunks, and each chunk not seen before is appended to one long stream. A chunk that repeats earlier content becomes a reference to the earlier copy. This is where deduplication happens.
3. **Compress.** The stream is split into fixed-size blocks, and each block is compressed on its own, across as many threads as there are cores. Each block tries every permitted codec, and the smallest result is stored.
4. **Write.** The metadata tables are encoded, compressed, checksummed and written ahead of the blocks.

The stream and the compressed blocks are held in scratch files in `$TMPDIR` while the build runs, not in memory. Expect the scratch space to need roughly the size of the deduplicated stream plus the finished image. If `$TMPDIR` is a tmpfs, that space is RAM.

## Block size

```sh
mkstipple ./app app.stipple --block-size 33554432
```

A block is the unit of compression and the unit of decoding. Larger blocks give the compressor more context and make smaller images. They also cost more to read, because a reader has to decode a whole block before it can return any byte from it.

The default is 32 MiB. Measured on a 1.4 GiB application tree at level 19:

| Block size | Image size |
|---|---|
| 16 MiB | 457.0 MiB |
| 32 MiB | 452.8 MiB |
| 64 MiB | 451.6 MiB |

The gain flattens out above 32 MiB while the cost of the first read keeps growing. Smaller trees, and anything read randomly rather than at start-up, are better served by smaller blocks. A data set read by many concurrent clients in small random pieces may want blocks of 1 MiB or less. [Tuning](/guide/tuning) has more on choosing.

The block size must be a power of two, at least 4096.

## Deduplication chunks

```sh
mkstipple ./app app.stipple --chunk-size 4096
```

Content is cut into chunks whose boundaries are chosen by the content itself, using FastCDC. Inserting a few bytes into a file moves only the boundaries close to the insertion, so two files that share most of their content still share most of their chunks.

`--chunk-size` sets the target average. Chunks vary between a quarter of it and eight times it. Smaller chunks find shorter duplicated runs, at the cost of more extents to record for each file. The default of 4 KiB works well for application trees, where similar libraries, locale files and icons share many short runs. Very large media files with little repetition gain nothing from small chunks and can use a larger value.

## File order

```sh
mkstipple ./app app.stipple --order ext
```

The order files are placed in decides which files share a block, and the compressor does best when similar files sit together.

| Order | Effect |
|---|---|
| `ext` (default) | Groups files by extension, then by name. All the shared libraries sit together, then all the Python files, and so on. Usually the smallest image. |
| `walk` | Places files in the order a sorted walk of the tree visits them. This is the format's canonical order. |

A [hot list](/guide/hot-lists) overrides both for the files it names, which go first in the order given.

## Compression level

```sh
mkstipple ./app app.stipple --level 19
```

Levels run from 1 to 22, and the default is 19. The level only affects build time. Decoding speed is much the same at every level.

| Level | Effect |
|---|---|
| 1 to 9 | Fast builds, useful while iterating. |
| 19 | The default. Close to the best ratio, at a reasonable build time. |
| 20 to 22 | A little smaller again, and markedly slower to build. A 64 MiB block image at level 22 came out at 449.4 MiB, against 451.6 MiB at level 19. |

`--no-compress` stores everything uncompressed, metadata included. That is mostly useful for testing, or for content that is already compressed.

## Executable filter

Every block is compressed twice: once as it is, and once after an x86 branch filter rewrites the relative targets of `call` and `jmp` instructions as absolute ones. In native code the same function is called from many places, and absolute targets repeat where relative ones never do. The smaller of the two encodings is stored, so a block with no machine code in it loses nothing but build time.

`--no-bcj` skips the filtered attempt. That about halves compression time, and is worth it for trees that contain no x86 executables or libraries.

## Dictionaries

With blocks of 1 MiB or less, `mkstipple` trains one Zstandard dictionary of up to 112 KiB from samples of the stream and offers it to every block. A dictionary lets small blocks reach a ratio close to much larger ones. Above 1 MiB a block has enough context of its own, and a dictionary only costs time, so none is trained.

`--dictionary-size` overrides the default, and `0` disables training.

## Metadata

Inodes, directory entries, names, extents and attributes are stored as compressed sections. For the Python standard library in the [quick start](/guide/quick-start), all of it together took 0.38 percent of the image. `--no-compress-metadata` stores it raw, which a reader without Zstandard support can still parse.

## Reproducible builds

Given the same tree and the same options, `mkstipple` writes the same bytes, whatever the thread count or memory limit. To make images from different checkouts or machines match, the modification times have to match too:

```sh
mkstipple ./app app.stipple --mtime zero
```

`--mtime zero` records one fixed time for every entry.

::: warning Byte-compiled caches check modification times
Python's `.pyc` files, and similar caches in other runtimes, record the modification time of their source and are thrown away when it does not match. Clamping every time to one value makes all of them stale, and the application recompiles on every launch. Keep `--mtime preserve`, the default, for application trees that ship such caches.
:::

`--order walk` without a hot list produces the format's canonical encoding, which any conforming writer reproduces exactly. The other orders are deterministic too, but specific to this writer.

## Threads and memory

```sh
mkstipple ./app app.stipple --threads 8 --memory 1073741824
```

`--threads` defaults to one per core. Each thread compresses one block at a time, so a build never uses more threads than there are blocks. A 69 MiB tree at 32 MiB blocks has only two to work on.

Each block in flight needs about two blocks' worth of memory for its input, its filtered copy and its output, plus the compressor's own state. With 32 MiB blocks on 16 threads, the 1.4 GiB example peaked at 2.7 GiB and took 86 seconds. `--memory` sets a rough ceiling by holding fewer blocks at once, never fewer than one. Neither option changes the image that is written.

## What cannot be stored

The format records directories, regular files, symbolic links, hard links, named pipes, sockets, and character and block devices. A name may contain any byte except `/` and NUL. Anything else stops the build with an error naming the path.
