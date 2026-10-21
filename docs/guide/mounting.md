# Mounting

<p class="lead">
<code>stipple mount</code> presents an image as a read-only directory through FUSE. Its job is to serve reads, and to have the bytes an application is about to read already decoded before it asks.
</p>

```sh
stipple mount app.stipple ~/mnt/app
```

The mount runs in the foreground and prints where it mounted. It ends when the mountpoint is unmounted, or when the process receives an interrupt, a termination signal or a hang-up. In each of those cases it unmounts cleanly first, so no disconnected mountpoint is left behind.

```sh
fusermount3 -u ~/mnt/app
```

Every option is listed in the [stipple reference](/reference/stipple#mount).

## What the kernel sees

The mount is read-only, and every attempt to change it fails with a read-only filesystem error. Permissions, ownership, link counts, modification times and extended attributes are reported as recorded. An entry built with `--mtime zero` reports the Unix epoch.

An image never changes while mounted, so the kernel is told to trust every lookup, attribute and symbolic link target for as long as it likes, and lookups of names that do not exist are cached too. Language runtimes probe for many files that are not there. Each such probe crosses into the mount only once.

By default only the user who mounted the image can see into it. `--allow-other` opens it to every user, which requires `user_allow_other` in `/etc/fuse.conf` unless the mount is run as root.

## Warming

Images use large blocks because they compress well, and a large block is expensive to decode. Decoding a 32 MiB block takes tens of milliseconds, while the read that wanted it may be four kilobytes. An application that maps a shared library touches its pages at scattered offsets for as long as it runs. Keeping every block it might touch would cost memory in proportion to everything it ever reads, and dropping them would mean decoding the same blocks over and over.

The mount avoids both with **warming**. Background workers decode a block once, then store every file byte it holds directly into the kernel's page cache. From then on, reads of those files are answered by the kernel from memory without reaching the mount at all, and the decoded block is dropped at once. Page cache is memory the kernel already manages: under pressure it reclaims pages, and a reclaimed page is simply read again.

Workers pick up blocks from four triggers:

| Trigger | What gets queued |
|---|---|
| Mount | The blocks holding the image's [hot region](/guide/hot-lists), as soon as the mount exists. |
| Open | The blocks holding any part of a file the application opens that is not stored yet. |
| Read | The blocks a read touched. |
| Read-ahead | A few blocks past each read, to catch the next file in a hot list. |

A block something is waiting on jumps ahead of one queued in case it is wanted, so speculation never makes an application wait.

Warming is triggered by opening a file, not by looking one up. A Python interpreter looks up many more modules than it imports, and warming everything it looked at would decode most of the image.

### Warming options

| Option | Default | Effect |
|---|---|---|
| `--warm-workers` | one per core, at most 12 | Threads decoding blocks. `0` turns warming off and serves every read on demand. |
| `--warm-start` | the hot region | How many blocks at the front of the stream to warm at mount time. |
| `--warm-ahead` | 2 | Blocks past each read to queue. |

The cap of 12 workers applies only to the default. An explicit `--warm-workers` is used as given. On the 1.4 GiB example, launch time stopped improving at about 12 workers, and every extra worker holding a block it is decoding adds memory.

## The block cache

Reads the page cache cannot answer go through a cache of decoded blocks. Its budget defaults to eight blocks, and never less than 32 MiB:

| Block size | Default budget |
|---|---|
| 64 KiB to 4 MiB | 32 MiB |
| 16 MiB | 128 MiB |
| 32 MiB | 256 MiB |

`--cache-budget` sets it in bytes, and it cannot be smaller than one block. With warming on, most blocks pass through the cache only briefly, so the budget mostly matters for warming turned off, or for an access pattern that reads well past what was warmed.

Readers that want the same block at the same time wait for a single decode rather than each decoding it, and different blocks decode in parallel. When the cache is full, the least recently used block goes first. A block read by a long run of sequential reads is dropped ahead of blocks serving random reads, so streaming one large file does not evict everything else.

## Mapping instead of reading

```sh
stipple mount app.stipple ~/mnt/app --mmap
```

`--mmap` maps the image file instead of reading from it. Metadata is then searched in place rather than copied into the process, and stored blocks are lent out without a copy. It helps most for images with large metadata, or with blocks stored uncompressed.

## Images inside other files

`--offset` and `--length` mount an image that occupies part of a larger file:

```sh
stipple mount launcher --offset 1974272 ~/mnt/app
```

See [Embedding images](/guide/embedding).

## Threads

`--threads` sets how many threads answer requests from the kernel. The default of four is plenty, because the work that takes time, decoding, happens on the warming workers. Changing it made no measurable difference in testing.
