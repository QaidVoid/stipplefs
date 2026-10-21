# Quick start

<p class="lead">
Install the tools, build an image, verify it, mount it, and take it apart again. Five minutes, start to finish.
</p>

## Install

stipplefs is a Rust crate with two command-line tools. Build them from a checkout with every feature the tools use:

```sh
git clone https://github.com/QaidVoid/stipplefs
cd stipplefs
cargo install --path . --features cli,mount,zstd,dict,mmap
```

This puts `mkstipple` and `stipple` in `~/.cargo/bin`. The features are described in [Cargo features](/reference/features). Leave out `mount` if you never need to mount images, and the tools will not link against FUSE at all.

::: tip Mounting needs FUSE
Mounting works on Linux with the kernel's FUSE module and the `fusermount3` helper, which most distributions ship in a package called `fuse3`. Nothing else on the system is required.
:::

## Build an image

Point `mkstipple` at a directory and name the image to write. Here it packs Python's standard library:

```sh
mkstipple /usr/lib/python3.14 python.stipple
```

```text
building python.stipple from /usr/lib/python3.14: block size 33554432, level 19, 16 threads
[    0.7s] placing      3595/3595 files, 69.2 MiB read
[   11.1s] compressing  1/2 blocks, 21.8 -> 6.4 MiB (29.3%), 2.1 MiB/s
[   18.1s] compressing  2/2 blocks, 53.8 -> 12.3 MiB (22.9%), 3.1 MiB/s
[   18.3s] writing      image
wrote python.stipple (12959744 bytes) from /usr/lib/python3.14
  directories    301
  files          3595
  symlinks       1
```

69 MiB of files became a 12.4 MiB image. Deduplication removed 15 MiB before compression began, and Zstandard did the rest.

On a terminal the progress line updates in place. When output goes to a file or a pipe, a line is written every five seconds instead.

## Check it

`stipple check` prints the image's structure, then reads every file and verifies every digest:

```sh
stipple check python.stipple
```

```text
format          1.3
image length    12959744
stream length   56389916
block size      33554432
hot region      0 bytes
blocks          2
inodes          3897
...
  metadata            49433 bytes  (12.7 per inode, 0.38% of image)
directories     300
files           3595 (72555234 bytes)
symlinks        1
verified        72555234 bytes read and checksummed in 0.084 s (827.8 MB/s)
```

## Mount it

```sh
mkdir -p ~/mnt/python
stipple mount python.stipple ~/mnt/python
```

The mount runs in the foreground until it is unmounted. From another terminal, the tree is an ordinary read-only directory:

```sh
ls ~/mnt/python/asyncio
head -3 ~/mnt/python/this.py
```

Press <kbd>Ctrl</kbd>+<kbd>C</kbd> in the terminal running the mount to unmount it, or unmount it from anywhere with `fusermount3`:

```sh
fusermount3 -u ~/mnt/python
```

## Extract it

To get a plain directory back, with symbolic links, special files, modification times and extended attributes restored:

```sh
stipple extract python.stipple ./python-copy
```

`--subtree` limits extraction to one path inside the image:

```sh
stipple extract python.stipple ./just-asyncio --subtree asyncio
```

## Next steps

- [Building images](/guide/building-images) explains each `mkstipple` setting and when to change it.
- [Hot lists](/guide/hot-lists) shows how to record an application's start-up and make the image start faster.
- [Embedding images](/guide/embedding) covers images stored inside other files.
- [Using the library](/guide/library) reads and writes images from Rust.
