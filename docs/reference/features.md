# Cargo features

<p class="lead">
The crate is split so that a program pays only for what it uses. A reader embedded in another application can leave out the writer, the command-line tools and FUSE entirely.
</p>

| Feature | Default | Enables |
|---|---|---|
| `read` | yes | Opening images and reading from them. |
| `write` | yes | Building images. |
| `fs` | yes | Reading from files, and building from a directory on disk. |
| `zstd` | no | The Zstandard codecs and compressed metadata. Needed for nearly every real image. |
| `dict` | no | Training dictionaries when writing. Implies `zstd`. |
| `mmap` | no | `MmapSource`, which maps an image file and lends its bytes in place. |
| `mount` | no | `stipplefs::mount`, the FUSE filesystem. |
| `cli` | no | The `stipple` and `mkstipple` binaries. |
| `metrics` | no | Counters and timings on every read, reported by `stipple` when a mount ends or a bench finishes. Without it they compile away. |

## Common combinations

The command-line tools, as installed in the [quick start](/guide/quick-start):

```sh
cargo install --path . --features cli,mount,zstd,dict,mmap
```

A program that only reads images:

```toml
stipplefs = { git = "https://github.com/QaidVoid/stipplefs", default-features = false, features = ["read", "fs", "zstd"] }
```

A program that only writes them:

```toml
stipplefs = { git = "https://github.com/QaidVoid/stipplefs", default-features = false, features = ["write", "fs", "zstd", "dict"] }
```

## Without a codec

Without `zstd`, a reader can still open images whose blocks are all stored and whose metadata is uncompressed, which is what `mkstipple --no-compress` produces. An image that needs a codec the build lacks is refused when it is opened, not at the first read that would need it.

A writer built without `zstd` stores everything uncompressed.
