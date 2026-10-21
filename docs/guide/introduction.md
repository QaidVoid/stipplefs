# Introduction

<p class="lead">
stipplefs packs a directory tree into a single read-only image. The image is compressed, deduplicated and checksummed, and it can be mounted, extracted, or read directly from Rust.
</p>

It was built for one job in particular: shipping an application as one file and starting it from that file nearly as fast as from an unpacked folder. That shapes most of its design, but nothing in the format is specific to applications. Any tree that is written once and read many times is a good fit.

## What you get

**One file for a whole tree.** Directories, regular files, symbolic links, hard links, device nodes, named pipes, sockets, permissions, ownership, modification times and extended attributes are all recorded.

**Small images.** Content is cut into chunks along boundaries the content itself chooses, so identical runs of bytes are stored once even when they sit at different offsets in different files. The deduplicated stream is then compressed with Zstandard in large blocks. Blocks that contain x86 machine code are also tried through a branch filter that makes calls and jumps far easier to compress, and the smaller result wins.

**Fast starts.** A build can be told which files an application opens while it starts. Those files go first in the image, in the order they are opened, and the mount begins decoding them the moment it exists. Decoded bytes are pushed straight into the kernel's page cache, so the application finds them already in memory.

**Integrity.** The superblock, every metadata section and every block carries an XXH3 digest. A reader verifies each one before it trusts or returns anything covered by it.

**A real specification.** The [format](/format) is documented completely enough to write an independent reader, and the test suite contains one, built from the document rather than from this crate's code. A canonical encoding means two conforming writers given the same tree produce the same bytes.

## How it compares to a plain folder

The numbers below come from packing Blender 5.2, a 1.4 GiB application tree, with the default settings and a hot list, then launching it in the background.

| | Plain folder | stipplefs image |
|---|---|---|
| Size on disk | 1.4 GiB | 453 MiB |
| First launch | 0.50 s | 0.73 s, mount included |
| Later launches | 0.50 s | 0.50 s |
| Extra memory | none | about 437 MiB in the mount daemon while starting |

The first launch on a fresh mount pays for two things a folder does not have. The first block has to be decoded in full before any of it can be read, and the kernel meets every file and directory for the first time through FUSE. Once those are cached, launches run at folder speed. [Tuning](/guide/tuning) covers where the remaining time goes and which settings move it.

## Where it fits

stipplefs is a good choice when:

- You ship an application or a data set as one file and care how fast it opens.
- You want an image that can be embedded inside another file, such as a self-mounting executable.
- You need images that are byte-for-byte reproducible from the same input.
- You want to read files out of an image from Rust without mounting anything.

It is not the right tool when you need to write to the tree after it is built. Images are read-only by design. Overlaying a writable directory on top of a mount is the usual answer when an application insists on writing next to its own files.

## The pieces

| Piece | What it does |
|---|---|
| `mkstipple` | Builds an image from a directory. |
| `stipple` | Checks, mounts, extracts and benchmarks images. |
| The `stipplefs` crate | Reads and writes images from Rust, behind independent `read` and `write` features. |
| [The specification](/format) | Defines every byte of the format. |

Continue with the [quick start](/guide/quick-start) to build and mount a first image.
