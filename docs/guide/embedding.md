# Embedding images

<p class="lead">
Every offset inside an image is measured from the image's own first byte. An image can therefore sit anywhere inside a larger file, such as a launcher script, an executable or an archive, and be read in place without being copied out.
</p>

## Reading an image at an offset

Every `stipple` command takes `--offset` and `--length`:

```sh
stipple check  bundle --offset 8192
stipple mount  bundle ~/mnt/app --offset 8192
stipple extract bundle ./out --offset 8192
```

`--offset` is where the image starts. `--length` defaults to the rest of the file. The image records its own length, so bytes after it, such as a signature, do no harm, and `--length` is only needed to keep a reader from looking past a given point.

The offset has no alignment requirement. Placing the image at a multiple of 4096 keeps its sections and blocks page-aligned within the file, which suits both the page cache and `--mmap`.

## A self-mounting application

A launcher can be a shell script with an image appended to it. The script mounts the file it lives in, runs the application, and unmounts it again on the way out.

```sh
#!/bin/sh
# Everything from byte 8192 of this file onwards is a stipplefs image.
self=$(readlink -f "$0")
mnt=$(mktemp -d)
stipple mount "$self" "$mnt" --offset 8192 2>/dev/null &
pid=$!
while [ ! -e "$mnt/AppRun" ]; do
    kill -0 "$pid" 2>/dev/null || { echo "mount failed" >&2; exit 1; }
    sleep 0.01
done
"$mnt/AppRun" "$@"
status=$?
kill "$pid"
wait "$pid"
rmdir "$mnt"
exit "$status"
```

Pad the script to the offset it names, then append the image:

```sh
mkstipple ./app app.stipple --hot-list hot.txt
cp launcher.sh app.run
truncate -s 8192 app.run
cat app.stipple >> app.run
chmod +x app.run
./app.run --version
```

The shell reads a script as it runs it, and `exit` stops it before it reaches the padding, so the image bytes are never parsed as commands. Killing the mount unmounts it cleanly, so the launcher needs nothing more than `kill` to tidy up.

A launcher meant for other machines usually carries the `stipple` binary too. Place it between the script and the image, record both offsets in the script, and copy the binary out to a cache directory the first time it runs.

## From Rust

`Image::open_at` opens an image inside a file, and `Image::new` opens one inside any byte source, such as a buffer already in memory:

```rust
use stipplefs::read::Image;

let size = std::fs::metadata("bundle")?.len() - 8192;
let image = Image::open_at("bundle", 8192, size)?;
let reader = image.reader()?;
let ino = reader.resolve(b"share/app/config.toml").expect("present");
let bytes = reader.read_all(ino)?;
```

See [Using the library](/guide/library) for the rest of the reading API.
