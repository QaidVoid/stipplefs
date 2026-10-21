# Tuning

<p class="lead">
The defaults come from measuring one large application tree. This page shows where the time and memory go, so you can tell which setting to move when your tree or your priorities differ.
</p>

## Where a first launch spends its time

Blender 5.2 is a 1.4 GiB tree of native libraries, Python modules and data files. Packed with the defaults and a hot list, it comes to 453 MiB. Launching it in the background breaks down like this:

| Launch | Time | What it adds |
|---|---|---|
| From a plain folder | 0.50 s | The application's own start-up. |
| Again, on a mount that has served it once | 0.50 s | Nothing. Every byte and every lookup is already cached by the kernel. |
| On a fresh mount, after warming has finished | 0.60 s | The kernel meets each file and directory for the first time, through FUSE. |
| On a fresh mount, straight away | 0.73 s | Waiting for blocks that are still being decoded. |

The last two steps are the whole cost of using an image instead of a folder. Each has its own setting to turn.

**First-time lookups** cost about 0.1 s here. Every file the application opens, and every file a language runtime probes for and does not find, is a round trip through FUSE the first time. The kernel caches the answers after that. Any FUSE filesystem pays this cost, and no image setting changes it.

**Decoding** costs the rest. A block cannot be read until it is decoded in full, so the first file in the image waits for the whole first block. At 32 MiB that takes about 40 ms. Warming decodes the hot region with several blocks in flight at once, so the later blocks are usually ready by the time the application needs them.

## Choosing a block size

| Priority | Block size | Why |
|---|---|---|
| Smallest image for an application | 32 MiB, the default | Within one percent of 64 MiB, at half the first-block wait. |
| Smallest image, start-up is irrelevant | 64 MiB | A further 0.25 percent, and twice the decode time per block. |
| Random access by many readers | 1 MiB or less | Each read decodes little. A dictionary is trained automatically below 1 MiB. |
| Small tree, a few tens of megabytes | 1 to 4 MiB | Large blocks leave the build with few blocks to spread across threads. |

The image grows as blocks shrink. On the Blender tree, 16 MiB blocks made an image 1 percent larger than 32 MiB. Very small blocks cost several percent more.

## Warming workers

More workers decode more of the hot region at once. On a 16-thread machine, raising the limit from 8 to 12 workers took the cold launch from 0.78 s to 0.73 s. Above 12 it got no faster, and each worker holds the block it is decoding, so memory rose instead.

```sh
stipple mount app.stipple ~/mnt/app --warm-workers 4
```

On a machine shared with other work, fewer workers trade a little launch time for less contention. `--warm-workers 0` turns warming off. The mount then decodes blocks only when a read needs them, which uses the least memory and suits images that are mostly read in small random pieces.

## Memory

While the application starts, the mount holds:

- the decoded metadata, which is small next to the blocks,
- one block for each worker that is decoding,
- the block cache, up to its budget, which defaults to eight blocks.

In the Blender test that came to about 437 MiB at peak. Warmed blocks are dropped as soon as their bytes are in the page cache, so the steady state is much lower. The page cache itself belongs to the kernel, which reclaims it under memory pressure like any other cached file.

To use less, lower `--warm-workers`, set a smaller `--cache-budget`, or build with smaller blocks. The cache budget must hold at least one block.

## Build time and memory

Measured on the Python standard library from the [quick start](/guide/quick-start), 69 MiB of files, with 4 MiB blocks on 16 threads:

| Settings | Build time | Peak memory | Image |
|---|---|---|---|
| `--level 19`, the default | 3.3 s | 769 MiB | 13.1 MiB |
| `--level 19 --no-bcj` | 1.7 s | 732 MiB | 13.3 MiB |
| `--level 9` | 0.5 s | 284 MiB | 14.4 MiB |
| `--level 3` | 0.2 s | 143 MiB | 15.8 MiB |

The same tree with the default 32 MiB blocks built in 18.3 s, because two blocks kept only two threads busy, and came out at 12.4 MiB. A low level is the quickest way to iterate on a hot list or a layout, with a full build at the end.

`--threads` scales until every block is in flight. `--memory` caps memory by holding fewer blocks at once, at the cost of parallelism. Neither `--threads` nor `--memory` changes the image, so a build can be tuned to the machine without affecting what it produces.

## Measuring your own tree

`stipple bench` times random reads straight through the library, without FUSE, and reports throughput and latency percentiles:

```sh
stipple bench app.stipple --reads 2000 --read-size 4096
```

For start-up, nothing beats timing the real application on a fresh mount. Remount between runs, because a mount that has served the application once keeps everything cached.
