<p align="center">
  <img src="docs/public/logo.svg" alt="" width="128" height="128">
</p>

<h1 align="center">stipplefs</h1>

<p align="center">
  A read-only, compressed, deduplicating filesystem image format,<br>
  with a FUSE mount built for fast application start-up.
</p>

---

stipplefs packs a directory tree into a single file. The file is compressed, deduplicated and checksummed, and it can be mounted, extracted, or read directly from Rust. It was built to ship an application as one file and start it from that file nearly as fast as from an unpacked folder.

Packing Blender 5.2, a 1.4 GiB application tree, with the default settings:

| | Plain folder | stipplefs image |
|---|---|---|
| Size | 1.4 GiB | 453 MiB |
| First launch, mount included | 0.50 s | 0.73 s |
| Later launches | 0.50 s | 0.50 s |

## Features

- **Start-up first.** A hot list puts the files an application opens while starting at the front of the image. The mount decodes them in the background and stores their bytes straight into the kernel's page cache before the application asks.
- **Deduplication inside files.** Content-defined chunking stores shared runs of bytes once, even between files that differ.
- **Smaller executables.** Every block is also tried through an x86 branch filter, and the smaller encoding is kept.
- **Checksummed throughout.** The superblock, every metadata section and every block carry an XXH3 digest, verified before use.
- **Embeddable.** Every offset is relative to the image, so it can live inside a launcher, an executable or an archive.
- **Specified.** A normative [format specification](docs/format.md), checked by an independent reader in the test suite, with a canonical encoding for reproducible builds.

## Install

```sh
git clone https://github.com/QaidVoid/stipplefs
cd stipplefs
cargo install --path . --features cli,mount,zstd,dict,mmap
```

Mounting needs Linux with FUSE and the `fusermount3` helper, usually packaged as `fuse3`.

## Use

```sh
# Build an image.
mkstipple ./app app.stipple

# Build one that starts fast, from a list of the files the app opens first.
mkstipple ./app app.stipple --hot-list hot.txt

# Verify every byte.
stipple check app.stipple

# Mount it. Ctrl+C unmounts.
stipple mount app.stipple ~/mnt/app

# Or unpack it.
stipple extract app.stipple ./app-copy
```

From Rust:

```rust
use stipplefs::read::Image;

let image = Image::open("app.stipple")?;
let reader = image.reader()?;
let ino = reader.resolve(b"share/app/config.toml").expect("present");
let bytes = reader.read_all(ino)?;
```

## Documentation

The documentation lives in [`docs/`](docs) and is built with VitePress:

```sh
cd docs
bun install
bun run dev
```

It covers building images, mounting and warming, recording hot lists, embedding images in other files, tuning, the library, and the [format specification](docs/format.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
