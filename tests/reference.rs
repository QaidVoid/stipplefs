//! A deliberately slow, obviously correct reader, and property tests that
//! check the real reader against it.
//!
//! This implementation parses the image from `docs/format.md` directly. It
//! caches nothing, borrows nothing, and decodes a block from scratch every
//! time a byte of it is needed. It exists to be obviously right rather than
//! fast, so that the real reader's caching, borrowing and binary searches have
//! something independent to be checked against.

#![cfg(all(feature = "read", feature = "write", feature = "zstd"))]

use std::collections::BTreeMap;
use stipplefs::write::{Builder, Node, Options};

fn u16at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn u32at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn u64at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
fn digest(b: &[u8]) -> u64 {
    twox_hash::XxHash3_64::oneshot(b)
}

struct Reference {
    image: Vec<u8>,
    block_size: u64,
    span_width: usize,
    stream_length: u64,
    inode_count: u32,
    /// Every section, verified once when the image is opened.
    sections: Vec<Vec<u8>>,
    /// The most recently decoded block. Decoding is otherwise repeated for
    /// every byte, which is correct but unusably slow.
    last: std::cell::RefCell<Option<(usize, Vec<u8>)>>,
}

impl Reference {
    fn open(image: Vec<u8>) -> Reference {
        assert_eq!(&image[0..8], b"STIPPLE\0", "magic");
        assert_eq!(u16at(&image, 8), 1, "major version");
        assert_eq!(
            digest(&image[..4088]),
            u64at(&image, 4088),
            "superblock digest"
        );

        let mut sections = Vec::new();
        for i in 0..8 {
            let base = 56 + i * 16;
            let off = u64at(&image, base);
            let size = u64at(&image, base + 8);
            if size == 0 {
                sections.push(Vec::new());
                continue;
            }
            let body = image[off as usize..(off + size - 8) as usize].to_vec();
            let stored = u64at(&image, (off + size - 8) as usize);
            assert_eq!(digest(&body), stored, "section {i} digest");
            sections.push(body);
        }

        Reference {
            block_size: u32at(&image, 32) as u64,
            span_width: u32at(&image, 52) as usize,
            stream_length: u64at(&image, 24),
            inode_count: u32at(&image, 36),
            sections,
            last: std::cell::RefCell::new(None),
            image,
        }
    }

    fn section(&self, i: usize) -> &[u8] {
        &self.sections[i]
    }

    /// Read one inode from the parallel arrays, following `docs/format.md`
    /// directly rather than reusing anything from the crate.
    ///
    /// Returns size, payload, extent count, mode, uid, gid and link count.
    fn inode(&self, ino: u32) -> (u64, u64, u32, u32, u32, u32, u32) {
        assert!(ino < self.inode_count);
        let s = self.section(0);
        let (ws, wp, wa, wn, wm, wx, we) = (s[0], s[1], s[2], s[3], s[4], s[5], s[6]);
        let n = self.inode_count as usize;
        let body = &s[16..];

        // Each array follows the previous one, so walking to a field means
        // stepping past every array before it.
        let read = |base: usize, width: u8, i: usize| -> u64 {
            if width == 0 {
                return 0;
            }
            let at = base + i * width as usize;
            let mut v = [0u8; 8];
            v[..width as usize].copy_from_slice(&body[at..at + width as usize]);
            u64::from_le_bytes(v)
        };
        let i = ino as usize;
        let mut base = 0usize;
        let size = read(base, ws, i);
        base += n * ws as usize;
        let payload = read(base, wp, i);
        base += n * wp as usize;
        let attrs = read(base, wa, i) as usize;
        base += n * wa as usize;
        let nlink = read(base, wn, i) as u32;
        base += n * wn as usize;
        base += n * wm as usize;
        base += n * wx as usize;
        let extent_count = read(base, we, i) as u32;

        let attr_section = self.section(6);
        let at = attrs * 12;
        let mode = u32at(attr_section, at);
        let uid = u32at(attr_section, at + 4);
        let gid = u32at(attr_section, at + 8);

        (size, payload, extent_count, mode, uid, gid, nlink)
    }

    fn decode_block(&self, index: usize) -> Vec<u8> {
        let blocks = self.section(4);
        let at = index * 20;
        let image_offset = u64at(blocks, at) as usize;
        let stored_length = u32at(blocks, at + 8) as usize;
        let decoded_length = u32at(blocks, at + 12) as usize;
        let codec = blocks[at + 16];

        let raw = &self.image[image_offset..image_offset + stored_length];
        let want = u64at(&self.image, image_offset + stored_length);
        assert_eq!(digest(raw), want, "block {index} digest");

        let decoded: Vec<u8> = match codec {
            0 => raw.to_vec(),
            1 => zstd::decode_all(raw).expect("zstd block"),
            2 => {
                let dict = self.section(5);
                let mut d = zstd::bulk::Decompressor::with_dictionary(dict).unwrap();
                d.decompress(raw, decoded_length).expect("zstd-dict block")
            }
            3 => {
                // A span block is decoded against every block before it in its
                // span. The first block of a span never refers back.
                let width = self.span_width;
                assert!(width >= 2, "span block in an image with no span width");
                let start = index - index % width;
                assert!(start < index, "the first block of a span cannot refer back");
                let mut context = Vec::new();
                for at in start..index {
                    context.extend_from_slice(&self.decode_block(at));
                }
                let mut d = zstd::bulk::Decompressor::with_dictionary(&context).unwrap();
                d.decompress(raw, decoded_length).expect("zstd-span block")
            }
            other => panic!("unknown codec {other}"),
        };
        assert_eq!(
            decoded.len(),
            decoded_length,
            "block {index} decoded length"
        );
        decoded
    }

    fn stream_byte(&self, offset: u64) -> u8 {
        assert!(offset < self.stream_length);
        let index = (offset / self.block_size) as usize;
        let within = (offset % self.block_size) as usize;
        let mut last = self.last.borrow_mut();
        if last.as_ref().map(|(i, _)| *i) != Some(index) {
            *last = Some((index, self.decode_block(index)));
        }
        last.as_ref().unwrap().1[within]
    }

    /// Read a whole file one byte at a time, resolving each byte from scratch.
    fn read_file(&self, ino: u32) -> Vec<u8> {
        let (size, payload, extent_count, ..) = self.inode(ino);
        let mut out = Vec::with_capacity(size as usize);
        for file_offset in 0..size {
            let stream_offset = if extent_count == 1 {
                payload + file_offset
            } else {
                let extents = self.section(3);
                let mut found = None;
                for k in 0..extent_count {
                    let at = (payload as u32 + k) as usize * 24;
                    let so = u64at(extents, at);
                    let fo = u64at(extents, at + 8);
                    let len = u32at(extents, at + 16) as u64;
                    if file_offset >= fo && file_offset < fo + len {
                        found = Some(so + (file_offset - fo));
                        break;
                    }
                }
                found.expect("no extent covers the offset")
            };
            out.push(self.stream_byte(stream_offset));
        }
        out
    }

    fn children(&self, ino: u32) -> BTreeMap<Vec<u8>, u32> {
        let (size, payload, ..) = self.inode(ino);
        let dirents = self.section(1);
        let names = self.section(2);
        let mut out = BTreeMap::new();
        for i in 0..size {
            let at = (payload + i) as usize * 12;
            let child = u32at(dirents, at);
            let no = u32at(dirents, at + 4) as usize;
            let nl = u16at(dirents, at + 8) as usize;
            out.insert(names[no..no + nl].to_vec(), child);
        }
        out
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// Generate a tree with repeated names, shared content and varied sizes.
fn random_tree(seed: u64, opts: Options) -> (Vec<u8>, BTreeMap<Vec<u8>, Vec<u8>>) {
    let mut rng = Rng(seed | 1);
    let vocab: [&[u8]; 6] = [b"a", b"bb", b"ccc", b"dir", b"sub", b"x"];

    // Prefixes are shared between files while every tail is unique, so
    // deduplication leaves files made of several extents rather than one.
    let prefixes: Vec<Vec<u8>> = (0..3)
        .map(|k| (0..(6000 + k * 2000)).map(|_| rng.next() as u8).collect())
        .collect();

    let mut b = Builder::new(opts).unwrap();
    let mut expected = BTreeMap::new();
    for _ in 0..24 {
        let depth = 1 + rng.below(3);
        let mut path = Vec::new();
        for _ in 0..depth {
            path.extend_from_slice(vocab[rng.below(vocab.len())]);
            path.push(b'/');
        }
        path.extend_from_slice(vocab[rng.below(vocab.len())]);
        path.extend_from_slice(format!("{}", rng.below(20)).as_bytes());

        let mut content = prefixes[rng.below(prefixes.len())].clone();
        let tail = 3000 + rng.below(9000);
        content.extend((0..tail).map(|_| rng.next() as u8));

        if b.insert(
            &path,
            Node::File {
                content: content.clone(),
                mode: 0o644,
                uid: 0,
                gid: 0,
                link_group: None,
            },
        )
        .is_ok()
        {
            expected.insert(path, content);
        }
    }
    (b.build().unwrap(), expected)
}

fn walk(r: &Reference, ino: u32, prefix: Vec<u8>, out: &mut BTreeMap<Vec<u8>, u32>) {
    for (name, child) in r.children(ino) {
        let mut p = prefix.clone();
        if !p.is_empty() {
            p.push(b'/');
        }
        p.extend_from_slice(&name);
        let (_, _, _, mode, ..) = r.inode(child);
        if mode & 0o170000 == 0o040000 {
            walk(r, child, p, out);
        } else {
            out.insert(p, child);
        }
    }
}

#[test]
fn the_reference_reader_agrees_with_the_real_one() {
    for seed in [1u64, 2, 3, 4, 5] {
        // Small blocks and chunks so that deduplication produces files made
        // of several extents, which is the resolution path worth checking.
        let opts = Options {
            block_size: 4096,
            chunk_average: 2048,
            level: 3,
            ..Options::default()
        };
        let (image, expected) = random_tree(seed, opts);
        let reference = Reference::open(image.clone());

        let img = stipplefs::read::Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let fast = img.reader().unwrap();

        let mut found = BTreeMap::new();
        walk(&reference, 0, Vec::new(), &mut found);
        assert_eq!(
            found.keys().cloned().collect::<Vec<_>>(),
            expected.keys().cloned().collect::<Vec<_>>(),
            "seed {seed}: paths differ"
        );

        let mut multi_extent = 0;
        for (path, ino) in &found {
            if fast.inode(*ino).unwrap().extent_count > 1 {
                multi_extent += 1;
            }
            let want = &expected[path];
            let slow = reference.read_file(*ino);
            let size = fast.inode(*ino).unwrap().size as usize;
            let mut quick = vec![0u8; size];
            fast.read(*ino, 0, &mut quick).unwrap();
            assert_eq!(&slow, want, "seed {seed}: reference disagrees with source");
            assert_eq!(&quick, want, "seed {seed}: reader disagrees with source");
        }
        assert!(
            multi_extent > 0,
            "seed {seed}: no multi-extent file, so that resolution path is untested"
        );
    }
}

#[test]
fn every_writer_option_combination_round_trips() {
    let mut checked = 0;
    for block_size in [4096u32, 65536] {
        for chunk_average in [4096u32, 65536] {
            for codecs in [
                stipplefs::format::Codec::Stored.bit(),
                stipplefs::format::Codec::Stored.bit() | stipplefs::format::Codec::Zstd.bit(),
                stipplefs::format::Codec::Stored.bit()
                    | stipplefs::format::Codec::Zstd.bit()
                    | stipplefs::format::Codec::ZstdDict.bit(),
            ] {
                for threads in [1usize, 4] {
                    for timestamps in [
                        stipplefs::write::Timestamps::Clamp,
                        stipplefs::write::Timestamps::Preserve,
                    ] {
                        for memory_limit in [None, Some(1 << 20), Some(64 << 20)] {
                            let opts = Options {
                                block_size,
                                chunk_average,
                                codecs,
                                threads,
                                memory_limit,
                                span_width: 4,
                                level: 3,
                                dictionary_size: 65536,
                                timestamps,
                            };
                            let (image, expected) = random_tree(9, opts);
                            let reference = Reference::open(image.clone());
                            let mut found = BTreeMap::new();
                            walk(&reference, 0, Vec::new(), &mut found);
                            for (path, ino) in &found {
                                assert_eq!(
                                    &reference.read_file(*ino),
                                    &expected[path],
                                    "block {block_size} chunk {chunk_average} codecs {codecs:#x} \
                                     threads {threads} timestamps {timestamps:?} \
                                     memory {memory_limit:?}"
                                );
                            }
                            checked += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(checked, 144);
}

/// A deliberately naive writer: no deduplication, no compression, one extent
/// per file, everything stored. Built directly from `docs/format.md` so that
/// the real reader is checked against an image it did not produce.
mod reference_writer {
    use super::digest;
    use std::collections::BTreeMap;

    pub struct Entry {
        pub content: Vec<u8>,
    }

    fn pad(v: &mut Vec<u8>) {
        while !v.len().is_multiple_of(4096) {
            v.push(0);
        }
    }

    /// Build an image holding a flat directory of files, in name order.
    pub fn build(files: &BTreeMap<Vec<u8>, Entry>, block_size: u32) -> Vec<u8> {
        let mut stream = Vec::new();
        let mut inodes: Vec<(u64, u64, u32, u32, u32)> = Vec::new();
        let mut dirents: Vec<[u8; 12]> = Vec::new();
        let mut names: Vec<u8> = Vec::new();

        // (size, payload, extent_count, mode, nlink)
        inodes.push((files.len() as u64, 0u64, 0u32, 0o040755u32, 2u32));

        for (name, entry) in files {
            let ino = inodes.len() as u32;
            let at = stream.len() as u64;
            stream.extend_from_slice(&entry.content);

            inodes.push((
                entry.content.len() as u64,
                at,
                if entry.content.is_empty() { 0u32 } else { 1 },
                0o100644u32,
                1u32,
            ));

            let mut d = [0u8; 12];
            d[0..4].copy_from_slice(&ino.to_le_bytes());
            d[4..8].copy_from_slice(&(names.len() as u32).to_le_bytes());
            d[8..10].copy_from_slice(&(name.len() as u16).to_le_bytes());
            dirents.push(d);
            names.extend_from_slice(name);
        }

        let blocks: Vec<&[u8]> = stream.chunks(block_size as usize).collect();
        // Widths wide enough for anything this naive writer produces.
        let mut inode_bytes: Vec<u8> = vec![8, 8, 1, 4, 0, 0, 4, 0];
        inode_bytes.extend_from_slice(&0u64.to_le_bytes());
        for (size, ..) in &inodes {
            inode_bytes.extend_from_slice(&size.to_le_bytes());
        }
        for (_, payload, ..) in &inodes {
            inode_bytes.extend_from_slice(&payload.to_le_bytes());
        }
        let mut modes: Vec<u32> = inodes.iter().map(|r| r.3).collect();
        modes.sort_unstable();
        modes.dedup();
        for r in &inodes {
            let at = modes.iter().position(|m| *m == r.3).unwrap();
            inode_bytes.push(at as u8);
        }
        for r in &inodes {
            inode_bytes.extend_from_slice(&r.4.to_le_bytes());
        }
        for r in &inodes {
            inode_bytes.extend_from_slice(&r.2.to_le_bytes());
        }
        let mut attr_bytes: Vec<u8> = Vec::new();
        for m in &modes {
            attr_bytes.extend_from_slice(&m.to_le_bytes());
            attr_bytes.extend_from_slice(&0u32.to_le_bytes());
            attr_bytes.extend_from_slice(&0u32.to_le_bytes());
        }
        let dirent_bytes: Vec<u8> = dirents.iter().flatten().copied().collect();

        let sized = |n: usize| -> u64 {
            if n == 0 {
                0
            } else {
                ((n as u64 + 8).div_ceil(4096)) * 4096
            }
        };
        let present: Vec<(usize, Vec<u8>)> = [
            (0usize, inode_bytes),
            (1, dirent_bytes),
            (2, names),
            (4, vec![0u8; blocks.len() * 20]),
            (6, attr_bytes),
        ]
        .into_iter()
        .filter(|(_, c)| !c.is_empty())
        .collect();

        let mut data_at = 4096u64;
        for (_, c) in &present {
            data_at += sized(c.len());
        }

        let mut index = Vec::new();
        let mut at = data_at;
        for b in &blocks {
            index.extend_from_slice(&at.to_le_bytes());
            index.extend_from_slice(&(b.len() as u32).to_le_bytes());
            index.extend_from_slice(&(b.len() as u32).to_le_bytes());
            index.push(0);
            index.extend_from_slice(&[0u8; 3]);
            at += b.len() as u64 + 8;
        }

        let mut placed = [(0u64, 0u64); 8];
        let mut body = Vec::new();
        let mut cursor = 4096u64;
        for (slot, content) in &present {
            let content = if *slot == 4 { &index } else { content };
            placed[*slot] = (cursor, content.len() as u64 + 8);
            body.extend_from_slice(content);
            body.extend_from_slice(&digest(content).to_le_bytes());
            pad(&mut body);
            cursor = 4096 + body.len() as u64;
        }
        for b in &blocks {
            body.extend_from_slice(b);
            body.extend_from_slice(&digest(b).to_le_bytes());
        }
        pad(&mut body);

        let total = 4096 + body.len() as u64;
        let mut sb = [0u8; 4096];
        sb[0..8].copy_from_slice(b"STIPPLE\0");
        sb[8..10].copy_from_slice(&1u16.to_le_bytes());
        sb[10..12].copy_from_slice(&1u16.to_le_bytes());
        sb[14..16].copy_from_slice(&1u16.to_le_bytes());
        sb[16..24].copy_from_slice(&total.to_le_bytes());
        sb[24..32].copy_from_slice(&(stream.len() as u64).to_le_bytes());
        sb[32..36].copy_from_slice(&block_size.to_le_bytes());
        sb[36..40].copy_from_slice(&(inodes.len() as u32).to_le_bytes());
        sb[44..48].copy_from_slice(&(blocks.len() as u32).to_le_bytes());
        for (i, (off, size)) in placed.iter().enumerate() {
            let base = 56 + i * 16;
            sb[base..base + 8].copy_from_slice(&off.to_le_bytes());
            sb[base + 8..base + 16].copy_from_slice(&size.to_le_bytes());
        }
        let d = digest(&sb[..4088]);
        sb[4088..].copy_from_slice(&d.to_le_bytes());

        let mut image = sb.to_vec();
        image.extend_from_slice(&body);
        image
    }
}

#[test]
fn the_real_reader_reads_the_reference_writers_output() {
    let mut rng = Rng(0xabcd);
    let mut files = BTreeMap::new();
    for i in 0..12u32 {
        let n = rng.below(9000);
        files.insert(
            format!("f{i:02}.bin").into_bytes(),
            reference_writer::Entry {
                content: (0..n).map(|_| rng.next() as u8).collect(),
            },
        );
    }
    files.insert(
        b"empty".to_vec(),
        reference_writer::Entry {
            content: Vec::new(),
        },
    );

    for block_size in [4096u32, 65536] {
        let image = reference_writer::build(&files, block_size);
        let img = stipplefs::read::Image::new(image.clone(), 0, image.len() as u64)
            .expect("real reader must open the reference writer's image");
        let r = img.reader().unwrap();

        let root = r.inode(0).unwrap();
        assert_eq!(root.size as usize, files.len());
        for i in 0..root.size as u32 {
            let e = r.dirent(root.payload as u32 + i).unwrap();
            let name = r.name(e).unwrap().to_vec();
            let want = &files[&name].content;
            let mut got = vec![0u8; want.len()];
            r.read(e.inode, 0, &mut got).unwrap();
            assert_eq!(
                &got,
                want,
                "block {block_size}: {} differs",
                String::from_utf8_lossy(&name)
            );
        }
    }
}
