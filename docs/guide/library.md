# Using the library

<p class="lead">
Everything the command-line tools do is available from the <code>stipplefs</code> crate. Reading and writing are separate features, so a program that only reads images carries no writer, and the reverse.
</p>

## Adding the crate

```toml
[dependencies]
stipplefs = { git = "https://github.com/QaidVoid/stipplefs", features = ["zstd"] }
```

The default features are `read`, `write` and `fs`. Add `zstd` to read or write compressed images, which is nearly all of them, and `dict` to train dictionaries when writing. A reader that only ever opens images built with `--no-compress` needs no codec at all. [Cargo features](/reference/features) lists every feature.

## Reading

```rust
use stipplefs::read::Image;

let image = Image::open("python.stipple")?;
let reader = image.reader()?;

let ino = reader.resolve(b"json/__init__.py").ok_or("not in the image")?;
let text = reader.read_all(ino)?;
```

`Image::open` validates the superblock. `reader()` verifies every metadata section once, so later reads never check those digests again. Each block's digest is checked when it is decoded.

Paths are byte strings, because file names on Linux are bytes. `resolve` walks a slash-separated path from the root and returns an inode number, or `None` when nothing is there.

### Reading part of a file

`read_all` suits small files. For anything large, read the range you need into your own buffer:

```rust
let mut head = [0u8; 64];
let n = reader.read(ino, 0, &mut head)?;
```

`read` returns how many bytes it filled. A read that starts inside the file and runs past its end returns the bytes that exist, and one that starts at or after the end returns zero.

### Walking directories

```rust
use stipplefs::format::FileType;

let dir = reader.resolve(b"json").ok_or("not in the image")?;
for (name, child) in reader.entries(dir) {
    let node = reader.inode(child).ok_or("bad inode")?;
    let kind = match node.file_type() {
        Some(FileType::Directory) => "dir",
        Some(FileType::Regular) => "file",
        Some(FileType::Symlink) => "link",
        _ => "other",
    };
    println!("{kind:5} {:>8} {}", node.size, String::from_utf8_lossy(name));
}
```

Entries come back sorted by name, and `lookup(dir, name)` finds one child by binary search. An inode carries its size, mode, ownership, link count and modification time. `symlink_target` and `xattrs` return a link's target and an inode's extended attributes.

### Where the bytes come from

`Image::open` reads a file with positioned reads. `Image::open_at` does the same for an image at an offset inside a larger file. `Image::new` accepts anything implementing `ByteSource`, which includes `Vec<u8>` and `&[u8]`, so an image already in memory needs no file at all. With the `mmap` feature, `MmapSource` maps a file and lets the reader search metadata in place.

### The cache

`reader()` picks a cache budget suited to the image's block size. `reader_with_budget(bytes)` sets one explicitly. A `Reader` is `Sync`. Share one between threads rather than creating one per thread, so they share a cache and never decode the same block twice at the same time.

## Writing

```rust
use stipplefs::write::{Builder, FileOrder, Options, Progress};

let options = Options {
    block_size: 32 << 20,
    chunk_average: 4096,
    dictionary_size: 0,
    threads: std::thread::available_parallelism()?.get(),
    ..Options::default()
};
let mut builder = Builder::new(options)?;
builder.set_order(FileOrder::Extension);
builder.set_hot_list(["bin/app", "lib/libapp.so"]);
builder.set_progress(|p| {
    if let Progress::Compressing { blocks, total_blocks, .. } = p {
        eprintln!("compressed {blocks}/{total_blocks} blocks");
    }
});
builder.add_tree(std::path::Path::new("./app"))?;

let mut out = std::io::BufWriter::new(std::fs::File::create("app.stipple")?);
builder.build_into(&mut out)?;
```

`Options::default()` describes the canonical encoding: 64 KiB blocks, 64 KiB chunks, every codec, level 19 and a single thread. The settings above match what `mkstipple` uses by default. `build_into` streams the image to any writer. `build` returns it as a `Vec<u8>`, which is simpler for small trees.

Progress reports arrive from worker threads, one per block while compressing, so the callback should be cheap.

### Building a tree by hand

`add_tree` reads a directory from disk. To build an image from data that is not on disk, insert entries one at a time:

```rust
use stipplefs::write::{Builder, Node, Options};

let mut builder = Builder::new(Options::default())?;
builder.insert(
    b"etc/greeting",
    Node::File {
        content: b"hello\n".to_vec(),
        mode: 0o644,
        uid: 0,
        gid: 0,
        link_group: None,
    },
)?;
builder.insert(
    b"greeting",
    Node::Symlink { target: b"etc/greeting".to_vec(), uid: 0, gid: 0 },
)?;
builder.set_mtime(b"etc/greeting", 1_700_000_000);
let bytes = builder.build()?;
```

Parent directories are created as needed. Files that share a `link_group` become one inode with several names, as hard links do. `Node` also covers directories, named pipes, sockets and devices, and `set_xattrs` attaches extended attributes to any entry.

## Errors

Every fallible call returns `stipplefs::error::Result`. The error says what went wrong in terms of the format: a bad magic number, an unsupported version, a digest that did not match and what it covered, a field holding a value the format forbids, or an I/O failure underneath. `Error` implements `std::error::Error`, so `?` works in any function returning `Box<dyn Error>`.

## Mounting from your own program

With the `mount` feature, `stipplefs::mount::Mount` implements FUSE's filesystem trait for an image, warming included. `stipple mount` is a small wrapper around it, and the place to start when embedding a mount in another program.
