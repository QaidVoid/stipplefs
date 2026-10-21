# Hot lists

<p class="lead">
A hot list names the files an application reads while it starts, in the order it reads them. Those files go to the front of the image, and the mount starts decoding them before the application has asked for anything.
</p>

Without a hot list, the files an application needs at start-up are scattered through the image. Every block that holds one of them has to be decoded before start-up can finish, and each block is decoded only when a read reaches it. With a hot list, they are packed tightly into the first few blocks, in the order they are needed, and warming works through those blocks in parallel from the moment the image is mounted.

## Recording a start-up

The simplest way to make a hot list is to trace the application starting from an unpacked copy of the tree. On Linux, `strace` records every file it opens:

```sh
root=$(realpath ./app)
strace -f -qq -e trace=openat,execve -o trace.log ./app/AppRun --version
grep -v ENOENT trace.log \
  | grep -oE '"[^"]+"' | tr -d '"' \
  | sed -n "s|^$root/||p" \
  | awk '!seen[$0]++' > hot.txt
```

The pipeline keeps the paths that were opened successfully, strips the tree's own prefix so they are relative to its root, and drops repeats while keeping first-open order.

Run the application the way it is usually run. A start-up that loads a document, a plug-in or a theme reads files that a bare `--version` does not. When the usual run never exits on its own, a command line that makes it exit right after start-up gives the cleanest trace.

::: tip Paths through symbolic links
`strace` records the path the application asked for. If the tree is reached through a symbolic link, or the application resolves its own location, use the same spelling of the root in `sed` as appears in the trace.
:::

## Building with it

```sh
mkstipple ./app app.stipple --hot-list hot.txt
```

The file holds one path per line, relative to the source directory. Lines naming something other than a regular file in the tree are ignored, as are directories, so a raw trace like the one above can be used as it is. A path named twice keeps its first position.

The files the list names are placed first in the order given, and everything else follows in the usual order. `stipple check` shows how much of the stream they fill:

```text
hot region      3001221 bytes
```

That length is stored in the image. When it is mounted, warming begins with the blocks it covers, without any option on the command line.

## What it is worth

Measured by mounting the Python standard library from the [quick start](/guide/quick-start), then starting Python against it and importing `asyncio`, `json`, `email.mime.text` and `http.client`. The hot list was recorded from a run that imported only the first two. Each figure is the median of seven runs, each on a fresh mount, with the mount included.

| | Mount and start |
|---|---|
| Image without a hot list | 0.126 s |
| Image with a hot list | 0.099 s |
| Image with a hot list, warming off | 0.133 s |
| Plain folder, no mount | 0.052 s |

The standard library fits in two blocks, so there is not much for the hot list to gather. On a larger tree the start-up files are spread over many more blocks, which gives the hot list more to gather. Blender 5.2 with 32 MiB blocks and a hot list launches in 0.73 s, mount included, against 0.50 s from a plain folder.

A hot list does not have to be perfect. A file missing from the list is still read correctly, from wherever it sits in the image. Warming queues its blocks as soon as the application opens it, so it is only slower to arrive. A file named in the list that is never read costs a little space in the hot region and some decoding at mount time, nothing more.

## Hot lists and image size

Reordering files changes which files share a block, which changes how well each block compresses. A hot list mixes file types at the front of the stream, so the image can come out slightly larger than one built with `--order ext` alone. The hot region is usually a small part of the stream, which keeps the effect small.

## Hot lists and the canonical encoding

An image built with a hot list is valid, but it is not in the format's canonical encoding, because the canonical order is a tree walk. It is still reproducible: the same tree, hot list and options produce the same bytes.
