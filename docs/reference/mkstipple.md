# mkstipple

<p class="lead">Build a stipplefs image from a directory tree.</p>

```sh
mkstipple [OPTIONS] <SOURCE> <OUTPUT>
```

| Argument | Meaning |
|---|---|
| `SOURCE` | Directory to build from. |
| `OUTPUT` | Image to write. |

## Layout

| Option | Default | Meaning |
|---|---|---|
| `--block-size <BYTES>` | `33554432` (32 MiB) | Decoded size of a block, a power of two of at least 4096. Larger blocks compress better and cost more to decode on first access. |
| `--chunk-size <BYTES>` | `4096` | Target average deduplication chunk size. Small chunks find duplicates that larger ones straddle. |
| `--order <ORDER>` | `ext` | How files are ordered in the stream. `ext` groups them by extension and name, which compresses better. `walk` uses sorted tree-walk order, the format's canonical encoding. |
| `--hot-list <FILE>` | none | A file naming, one path per line relative to the source, files to place first in the stream, in that order. See [Hot lists](/guide/hot-lists). |

## Compression

| Option | Default | Meaning |
|---|---|---|
| `--level <LEVEL>` | `19` | Zstandard level from 1 to 22, for blocks and for compressed metadata. |
| `--no-compress` | off | Store everything uncompressed, metadata included. |
| `--no-bcj` | off | Skip the x86 branch filter, which drops both filtered codecs. |
| `--no-compress-metadata` | off | Store metadata sections uncompressed. |
| `--dictionary-size <BYTES>` | 112 KiB up to 1 MiB blocks, none above | Largest dictionary to train. `0` disables training. |

## Metadata

| Option | Default | Meaning |
|---|---|---|
| `--mtime <MODE>` | `preserve` | `preserve` records each entry's own modification time. `zero` records one fixed value for every entry, which makes builds of the same content byte-identical across machines. |

## Resources

| Option | Default | Meaning |
|---|---|---|
| `--threads <N>` | one per core | Worker threads compressing blocks. Output does not depend on this. |
| `--memory <BYTES>` | none | Rough ceiling on memory used to compress blocks. Cannot go below one block. Output does not depend on this. |

## Output

While it runs, `mkstipple` reports which stage it is in: placing files, training a dictionary, compressing blocks, or writing the image. On a terminal the report is one line updated in place. Otherwise a line is written every five seconds. At the end it prints the image size and how many entries of each kind went in.

Progress and the summary go to standard error. The exit status is zero on success and non-zero on any error, which is printed with the path it concerns.

## Examples

Build with the defaults:

```sh
mkstipple ./app app.stipple
```

Build for fast start-up:

```sh
mkstipple ./app app.stipple --hot-list hot.txt
```

Build quickly while iterating:

```sh
mkstipple ./app app.stipple --level 3
```

Build the canonical, reproducible encoding:

```sh
mkstipple ./tree tree.stipple --order walk --mtime zero
```
