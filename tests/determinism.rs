//! Determinism of the writer, checked across separate processes.
//!
//! Rust seeds hash containers per process, so two builds inside one process
//! share a seed and agree even when the writer is not deterministic. Only a
//! cross-process comparison can catch that.

#![cfg(all(feature = "read", feature = "write"))]

use std::process::Command;
use stipplefs::write::{Builder, Node, Options};

/// A tree whose names come from a small vocabulary and whose contents repeat,
/// so candidates genuinely tie. A tree of distinct names proves nothing.
fn fixture() -> Vec<u8> {
    build_fixture(Options::default())
}

/// The same tree with content-defined block ends, whose cut points must be
/// as reproducible as everything else.
fn variable_fixture() -> Vec<u8> {
    build_fixture(Options {
        variable_blocks: true,
        ..Options::default()
    })
}

fn build_fixture(options: Options) -> Vec<u8> {
    let vocab: [&[u8]; 8] = [
        b"src", b"lib", b"test", b"mod", b"util", b"core", b"data", b"main",
    ];
    let bodies: Vec<Vec<u8>> = (0..16)
        .map(|k| {
            let mut v = b"repeated body line for tie generation. ".repeat(300);
            v.extend_from_slice(format!("variant {k}\n").as_bytes());
            v.extend(b"a tail that differs a little. ".repeat(50 + k * 3));
            v
        })
        .collect();

    let mut b = Builder::new(options).unwrap();
    for i in 0..8usize {
        for j in 0..8usize {
            for (k, body) in bodies.iter().enumerate() {
                let mut path = Vec::new();
                path.extend_from_slice(vocab[i]);
                path.push(b'/');
                path.extend_from_slice(vocab[j]);
                path.push(b'/');
                path.extend_from_slice(vocab[(i + j) % 8]);
                path.extend_from_slice(format!("{k}").as_bytes());
                b.insert(
                    &path,
                    Node::File {
                        content: body.clone(),
                        mode: 0o644,
                        uid: 1000,
                        gid: 1000,
                        link_group: None,
                    },
                )
                .unwrap();
            }
        }
    }
    let mut noise_seed = 0x1234_5678_9abc_def0u64;
    for dir in vocab.iter().take(6) {
        let noise: Vec<u8> = (0..90_000)
            .map(|_| {
                noise_seed ^= noise_seed << 13;
                noise_seed ^= noise_seed >> 7;
                noise_seed ^= noise_seed << 17;
                (noise_seed >> 24) as u8
            })
            .collect();
        let mut path = dir.to_vec();
        path.extend_from_slice(b"/noise");
        b.insert(
            &path,
            Node::File {
                content: noise,
                mode: 0o644,
                uid: 0,
                gid: 0,
                link_group: None,
            },
        )
        .unwrap();
    }
    for i in 0..8usize {
        let mut path = vocab[i].to_vec();
        path.extend_from_slice(b"/link");
        b.insert(
            &path,
            Node::Symlink {
                target: vocab[(i + 3) % 8].to_vec(),
                uid: 0,
                gid: 0,
            },
        )
        .unwrap();
    }
    b.build().unwrap()
}

fn digest(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{:016x}:{}", h, bytes.len())
}

#[test]
fn the_fixture_exercises_every_path_that_could_vary() {
    let image = fixture();
    let img = stipplefs::read::Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let sb = *img.superblock();
    let r = img.reader().unwrap();

    // Repeated names must be interned once, so the name table is far smaller
    // than the sum of the name lengths that reference it.
    let mut total_name_bytes = 0usize;
    let mut stack = vec![0u32];
    let mut files = 0usize;
    let mut raw_bytes = 0u64;
    while let Some(ino) = stack.pop() {
        let node = r.inode(ino).unwrap();
        if node.file_type() == Some(stipplefs::format::FileType::Directory) {
            for i in 0..node.size as u32 {
                let e = r.dirent(node.payload as u32 + i).unwrap();
                total_name_bytes += e.name_length as usize;
                stack.push(e.inode);
            }
        } else if node.file_type() == Some(stipplefs::format::FileType::Regular) {
            files += 1;
            raw_bytes += node.size;
        }
    }
    let name_table = img
        .section_data(stipplefs::format::SectionKind::Names)
        .unwrap()
        .len();
    assert!(files >= 100, "fixture should hold many files, got {files}");
    assert!(
        name_table < total_name_bytes / 2,
        "names are not being shared: table {name_table}, references {total_name_bytes}"
    );

    // Deduplication must fire, or the global index is untested.
    assert!(
        sb.stream_length < raw_bytes / 2,
        "deduplication did not fire: stream {} vs raw {raw_bytes}",
        sb.stream_length
    );

    // More than one block, and more than one codec, so block choice is exercised.
    let idx = img.block_index().unwrap();
    assert!(idx.count() > 1, "only {} block(s)", idx.count());
    let codecs: std::collections::BTreeSet<u8> = (0..idx.count())
        .map(|i| idx.get(i).unwrap().codec.id())
        .collect();
    assert!(
        codecs.len() > 1,
        "only one codec used, so codec selection is untested: {codecs:?}"
    );

    // The default image must exercise both new format features, or the paths
    // that write them are untested here.
    assert_ne!(
        sb.flags & stipplefs::format::FLAG_COMPRESSED_METADATA,
        0,
        "metadata is not compressed by default"
    );
}

#[test]
fn x86_shaped_content_selects_a_bcj_codec_deterministically() {
    use stipplefs::format::Codec;

    let tree = || {
        let mut code = Vec::new();
        for p in (0..(64 << 10)).step_by(8) {
            let rel = 0x1000i32 - (p + 5);
            code.push(0xE8u8);
            code.extend_from_slice(&rel.to_le_bytes());
            code.extend_from_slice(&[0x90, 0x90, 0x90]);
        }
        let mut b = Builder::new(Options::default()).unwrap();
        b.insert(
            b"code.bin",
            Node::File {
                content: code,
                mode: 0o644,
                uid: 0,
                gid: 0,
                link_group: None,
            },
        )
        .unwrap();
        b.build().unwrap()
    };

    let first = tree();
    let second = tree();
    assert_eq!(first, second, "BCJ blocks are not deterministic");

    let img = stipplefs::read::Image::new(first.clone(), 0, first.len() as u64).unwrap();
    let sb = *img.superblock();
    assert_ne!(
        sb.codec_mask & (Codec::ZstdBcj.bit() | Codec::ZstdDictBcj.bit()),
        0,
        "x86-shaped content did not select a BCJ codec: mask {:#06x}",
        sb.codec_mask
    );

    let r = img.reader().unwrap();
    let ino = r.resolve(b"code.bin").unwrap();
    let want = r.inode(ino).unwrap().size as usize;
    let mut buf = vec![0u8; want];
    assert_eq!(r.read(ino, 0, &mut buf).unwrap(), want);
    assert_eq!(buf[..8], [0xE8, 0xFB, 0x0F, 0, 0, 0x90, 0x90, 0x90]);
}

#[test]
fn repeated_builds_in_one_process_agree() {
    assert_eq!(digest(&fixture()), digest(&fixture()));
    assert_eq!(digest(&variable_fixture()), digest(&variable_fixture()));
}

#[test]
fn writes_are_byte_identical_across_processes() {
    if std::env::var_os("STIPPLEFS_DETERMINISM_CHILD").is_some() {
        println!(
            "STIPPLEFS_DIGEST={}/{}",
            digest(&fixture()),
            digest(&variable_fixture())
        );
        return;
    }

    let exe = std::env::current_exe().expect("test binary path");
    let mut seen = Vec::new();
    for _ in 0..4 {
        let out = Command::new(&exe)
            .args([
                "writes_are_byte_identical_across_processes",
                "--exact",
                "--nocapture",
            ])
            .env("STIPPLEFS_DETERMINISM_CHILD", "1")
            .output()
            .expect("spawn child");
        assert!(out.status.success(), "child failed: {:?}", out.status);
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text
            .lines()
            .find_map(|l| l.strip_prefix("STIPPLEFS_DIGEST="))
            .expect("child did not report a digest");
        seen.push(line.to_string());
    }
    assert!(
        seen.windows(2).all(|w| w[0] == w[1]),
        "writer output differs between processes: {seen:?}"
    );
}
