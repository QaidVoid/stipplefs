//! Writer to reader round trips, exercised only through the public API.

#![cfg(all(feature = "read", feature = "write"))]

use stipplefs::format::{Codec, FileType};
use stipplefs::read::{Image, Reader};
use stipplefs::write::{Builder, Node, Options};

fn file(content: &[u8]) -> Node {
    Node::File {
        content: content.to_vec(),
        mode: 0o644,
        uid: 1000,
        gid: 1000,
        link_group: None,
    }
}

fn linked(content: &[u8], group: u64) -> Node {
    Node::File {
        content: content.to_vec(),
        mode: 0o644,
        uid: 0,
        gid: 0,
        link_group: Some(group),
    }
}

fn pattern(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        })
        .collect()
}

fn read_all(r: &Reader<'_>, ino: u32) -> Vec<u8> {
    let size = r.inode(ino).unwrap().size as usize;
    let mut buf = vec![0u8; size];
    let n = r.read(ino, 0, &mut buf).unwrap();
    assert_eq!(n, size);
    buf
}

fn lookup(r: &Reader<'_>, dir: u32, name: &[u8]) -> Option<u32> {
    let d = r.inode(dir)?;
    (0..d.size as u32).find_map(|i| {
        let e = r.dirent(d.payload as u32 + i)?;
        (r.name(e)? == name).then_some(e.inode)
    })
}

#[test]
fn a_small_tree_round_trips() {
    let mut b = Builder::new(Options::default()).unwrap();
    let alpha = pattern(300_000, 1);
    let beta = pattern(5000, 2);
    b.insert(b"dir/alpha.bin", file(&alpha)).unwrap();
    b.insert(b"dir/beta.bin", file(&beta)).unwrap();
    b.insert(b"empty.bin", file(b"")).unwrap();
    b.insert(
        b"dir/link",
        Node::Symlink {
            target: b"alpha.bin".to_vec(),
            uid: 0,
            gid: 0,
        },
    )
    .unwrap();
    let image = b.build().unwrap();

    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let r = img.reader().unwrap();

    let dir = lookup(&r, 0, b"dir").unwrap();
    assert_eq!(r.inode(dir).unwrap().file_type(), Some(FileType::Directory));

    assert_eq!(read_all(&r, lookup(&r, dir, b"alpha.bin").unwrap()), alpha);
    assert_eq!(read_all(&r, lookup(&r, dir, b"beta.bin").unwrap()), beta);

    let e = lookup(&r, 0, b"empty.bin").unwrap();
    assert_eq!(r.inode(e).unwrap().size, 0);

    let l = lookup(&r, dir, b"link").unwrap();
    assert_eq!(r.inode(l).unwrap().file_type(), Some(FileType::Symlink));
}

#[test]
fn directory_entries_come_back_sorted_by_name() {
    let mut b = Builder::new(Options::default()).unwrap();
    for name in [b"zeta".as_slice(), b"alpha", b"Mid", b"beta", b"_under"] {
        b.insert(name, file(b"x")).unwrap();
    }
    let image = b.build().unwrap();
    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let r = img.reader().unwrap();

    let root = r.inode(0).unwrap();
    let names: Vec<Vec<u8>> = (0..root.size as u32)
        .map(|i| {
            r.name(r.dirent(root.payload as u32 + i).unwrap())
                .unwrap()
                .to_vec()
        })
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert_eq!(names[0], b"Mid".to_vec());
}

#[test]
fn identical_content_is_stored_once() {
    let content = pattern(400_000, 9);
    let mut one = Builder::new(Options::default()).unwrap();
    one.insert(b"a.bin", file(&content)).unwrap();
    let single = one.build().unwrap();

    let mut two = Builder::new(Options::default()).unwrap();
    two.insert(b"a.bin", file(&content)).unwrap();
    two.insert(b"b.bin", file(&content)).unwrap();
    two.insert(b"c.bin", file(&content)).unwrap();
    let triple = two.build().unwrap();

    let s = Image::new(single.clone(), 0, single.len() as u64).unwrap();
    let t = Image::new(triple.clone(), 0, triple.len() as u64).unwrap();
    assert_eq!(s.superblock().stream_length, t.superblock().stream_length);

    let r = t.reader().unwrap();
    for name in [b"a.bin".as_slice(), b"b.bin", b"c.bin"] {
        assert_eq!(read_all(&r, lookup(&r, 0, name).unwrap()), content);
    }
}

#[test]
fn partially_shared_content_is_stored_once() {
    let shared = pattern(200_000, 21);
    let mut a = shared.clone();
    a.extend_from_slice(&pattern(50_000, 22));
    let mut c = shared.clone();
    c.extend_from_slice(&pattern(50_000, 23));

    let mut b = Builder::new(Options::default()).unwrap();
    b.insert(b"a.bin", file(&a)).unwrap();
    b.insert(b"c.bin", file(&c)).unwrap();
    let image = b.build().unwrap();

    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let stream = img.superblock().stream_length;
    assert!(
        stream < (a.len() + c.len()) as u64,
        "shared prefix should be stored once: stream {stream} vs {} raw",
        a.len() + c.len()
    );

    let r = img.reader().unwrap();
    assert_eq!(read_all(&r, lookup(&r, 0, b"a.bin").unwrap()), a);
    assert_eq!(read_all(&r, lookup(&r, 0, b"c.bin").unwrap()), c);
}

#[test]
fn hard_links_share_an_inode_but_identical_files_do_not() {
    let content = pattern(20_000, 31);
    let mut b = Builder::new(Options::default()).unwrap();
    b.insert(b"one", linked(&content, 7)).unwrap();
    b.insert(b"two", linked(&content, 7)).unwrap();
    b.insert(b"copy", file(&content)).unwrap();
    let image = b.build().unwrap();

    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let r = img.reader().unwrap();

    let one = lookup(&r, 0, b"one").unwrap();
    let two = lookup(&r, 0, b"two").unwrap();
    let copy = lookup(&r, 0, b"copy").unwrap();

    assert_eq!(one, two, "hard links must share an inode");
    assert_ne!(one, copy, "distinct files must not share an inode");
    assert!(r.inode(one).unwrap().nlink >= 2);
    assert_eq!(r.inode(copy).unwrap().nlink, 1);
    assert_eq!(read_all(&r, one), content);
    assert_eq!(read_all(&r, copy), content);
}

#[test]
fn incompressible_content_is_stored_uncompressed() {
    let noise = pattern(300_000, 41);
    let mut b = Builder::new(Options::default()).unwrap();
    b.insert(b"noise.bin", file(&noise)).unwrap();
    let image = b.build().unwrap();

    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let idx = img.block_index().unwrap();
    let stored = (0..idx.count())
        .filter(|i| idx.get(*i).unwrap().codec == Codec::Stored)
        .count();
    assert!(stored > 0, "random data should not compress");

    let r = img.reader().unwrap();
    assert_eq!(read_all(&r, lookup(&r, 0, b"noise.bin").unwrap()), noise);
}

#[test]
fn a_restricted_codec_set_is_respected() {
    let text = b"the same short line, over and over. ".repeat(20_000);
    let opts = Options {
        codecs: Codec::Stored.bit(),
        ..Options::default()
    };
    let mut b = Builder::new(opts).unwrap();
    b.insert(b"text", file(&text)).unwrap();
    let image = b.build().unwrap();

    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    assert_eq!(img.superblock().codec_mask, Codec::Stored.bit());
    let idx = img.block_index().unwrap();
    for i in 0..idx.count() {
        assert_eq!(idx.get(i).unwrap().codec, Codec::Stored);
    }
    let r = img.reader().unwrap();
    assert_eq!(read_all(&r, lookup(&r, 0, b"text").unwrap()), text);
}

#[test]
fn block_and_chunk_sizes_are_configurable_and_round_trip() {
    let content = pattern(500_000, 51);
    for (block, chunk) in [(4096u32, 4096u32), (65536, 16384), (131072, 65536)] {
        let opts = Options {
            block_size: block,
            chunk_average: chunk,
            ..Options::default()
        };
        let mut b = Builder::new(opts).unwrap();
        b.insert(b"f.bin", file(&content)).unwrap();
        let image = b.build().unwrap();

        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        assert_eq!(img.superblock().block_size, block);
        let r = img.reader().unwrap();
        assert_eq!(read_all(&r, lookup(&r, 0, b"f.bin").unwrap()), content);
    }
}

#[test]
fn a_bad_block_size_is_rejected() {
    for bad in [5000u32, 2048, 0] {
        assert!(
            Builder::new(Options {
                block_size: bad,
                ..Options::default()
            })
            .is_err(),
            "block size {bad} should be rejected"
        );
    }
    // Large blocks are permitted. The format does not choose a point on the
    // curve between compression ratio and the cost of serving one small read.
    for ok in [1u32 << 24, 1 << 26, 1 << 30] {
        assert!(
            Builder::new(Options {
                block_size: ok,
                ..Options::default()
            })
            .is_ok(),
            "block size {ok} should be accepted"
        );
    }
}

#[test]
fn thread_count_does_not_change_the_output() {
    let make = |threads: usize| {
        let opts = Options {
            threads,
            block_size: 4096,
            chunk_average: 4096,
            ..Options::default()
        };
        let mut b = Builder::new(opts).unwrap();
        for i in 0..40u32 {
            let mut c = b"shared prefix that repeats a great deal. ".repeat(60);
            c.extend_from_slice(format!("tail {i}").as_bytes());
            c.extend_from_slice(&pattern(9000, i as u64 + 100));
            b.insert(format!("f{i:03}.bin").as_bytes(), file(&c))
                .unwrap();
        }
        b.build().unwrap()
    };
    let one = make(1);
    for threads in [2usize, 3, 4, 8, 16] {
        assert_eq!(make(threads), one, "output differs at {threads} threads");
    }

    let img = Image::new(one.clone(), 0, one.len() as u64).unwrap();
    assert!(
        img.block_index().unwrap().count() > 8,
        "test needs many blocks"
    );
}

#[cfg(feature = "dict")]
#[test]
fn a_trained_dictionary_is_used_and_round_trips() {
    let build = |dictionary_size: usize| {
        let opts = Options {
            dictionary_size,
            block_size: 4096,
            chunk_average: 4096,
            ..Options::default()
        };
        let mut b = Builder::new(opts).unwrap();
        for i in 0..64u32 {
            let mut c = b"a highly repetitive body shared across every file. ".repeat(40);
            c.extend_from_slice(format!("unique tail number {i}\n").as_bytes());
            b.insert(format!("f{i:03}.txt").as_bytes(), file(&c))
                .unwrap();
        }
        b.build().unwrap()
    };

    let with_dict = build(112 * 1024);
    let without = build(0);

    let a = Image::new(with_dict.clone(), 0, with_dict.len() as u64).unwrap();
    let b = Image::new(without.clone(), 0, without.len() as u64).unwrap();

    assert_ne!(
        a.superblock()
            .section(stipplefs::format::SectionKind::Dictionaries)
            .size,
        0,
        "a dictionary should have been trained and stored"
    );
    assert_eq!(
        b.superblock()
            .section(stipplefs::format::SectionKind::Dictionaries)
            .size,
        0,
        "no dictionary was requested"
    );
    assert_ne!(
        a.superblock().codec_mask & (Codec::ZstdDict.bit() | Codec::ZstdDictBcj.bit()),
        0
    );

    let ra = a.reader().unwrap();
    let rb = b.reader().unwrap();
    for i in 0..64u32 {
        let name = format!("f{i:03}.txt");
        let ia = lookup(&ra, 0, name.as_bytes()).unwrap();
        let ib = lookup(&rb, 0, name.as_bytes()).unwrap();
        assert_eq!(read_all(&ra, ia), read_all(&rb, ib), "{name} differs");
    }
}

#[cfg(feature = "dict")]
#[test]
fn dictionary_training_is_reproducible() {
    let build = || {
        let opts = Options {
            block_size: 4096,
            chunk_average: 4096,
            ..Options::default()
        };
        let mut b = Builder::new(opts).unwrap();
        for i in 0..64u32 {
            let mut c = b"repeated content for dictionary training. ".repeat(40);
            c.extend_from_slice(format!("{i}\n").as_bytes());
            b.insert(format!("f{i:03}.txt").as_bytes(), file(&c))
                .unwrap();
        }
        b.build().unwrap()
    };
    assert_eq!(build(), build());
}
