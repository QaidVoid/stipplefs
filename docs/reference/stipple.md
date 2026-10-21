# stipple

<p class="lead">Inspect, mount and extract stipplefs images.</p>

```sh
stipple <COMMAND> [OPTIONS]
```

| Command | What it does |
|---|---|
| [`check`](#check) | Report an image's structure and verify its integrity. |
| [`mount`](#mount) | Mount an image on a directory. |
| [`extract`](#extract) | Write an image's contents to a directory. |
| [`bench`](#bench) | Time random reads served directly by the library, without FUSE. |

## Options every command takes

| Option | Default | Meaning |
|---|---|---|
| `--offset <BYTES>` | `0` | Byte offset of the image within the file. |
| `--length <BYTES>` | rest of the file | Length of the image in bytes. |
| `--cache-budget <BYTES>` | eight blocks, at least 32 MiB | Bytes of decoded blocks the reader may retain. Cannot be smaller than one block. |
| `--mmap` | off | Map the image instead of reading it, so blocks are taken in place. |

## check

```sh
stipple check <PATH>
```

Prints the superblock fields, each section's size and position, the count of every kind of entry, and how much of the image metadata takes up. Then it reads every file and verifies every digest, and reports the throughput. A damaged image fails with an error naming what did not match.

## mount

```sh
stipple mount <PATH> <MOUNTPOINT>
```

Mounts the image read-only and serves it until the mountpoint is unmounted. An interrupt, a termination signal or a hang-up unmounts it cleanly before the process exits. See [Mounting](/guide/mounting).

| Option | Default | Meaning |
|---|---|---|
| `--allow-other` | off | Let other users see the mount. Needs `user_allow_other` in `/etc/fuse.conf` when not run as root. |
| `--threads <N>` | `4` | Threads answering requests from the kernel. |
| `--warm-workers <N>` | one per core, at most 12 | Background threads decoding blocks and storing their file bytes in the kernel's page cache. The cap applies only to the default. `0` serves every read on demand. |
| `--warm-ahead <N>` | `2` | Blocks past each read to warm ahead of it. |
| `--warm-start <N>` | the hot region | Blocks at the front of the stream to warm as soon as the mount exists. |

## extract

```sh
stipple extract <PATH> <DESTINATION>
```

Writes the tree into `DESTINATION`, creating it if needed. Regular files, directories, symbolic links, named pipes, sockets and devices are recreated, along with modification times and extended attributes. Creating devices needs the privileges `mknod` needs.

| Option | Meaning |
|---|---|
| `--subtree <PATH>` | Extract only this path within the image. |

## bench

```sh
stipple bench <PATH>
```

Picks random reads across the image's files and times them through the library, once from a cold cache and once again warm. Reports how long the image took to open and to serve its first read, then the throughput and the 50th, 90th and 99th percentile latency of each pass.

| Option | Default | Meaning |
|---|---|---|
| `--reads <N>` | `2000` | Random reads to perform. |
| `--read-size <BYTES>` | `4096` | Size of each read. |
| `--seed <N>` | `20260908` | Seed, so a run can be repeated exactly. |
