//! Content-defined blocks: what stays stable across builds, and what a reader
//! refuses in a variable block index.

#![cfg(all(feature = "read", feature = "write", feature = "zstd"))]

use std::collections::HashSet;
use stipplefs::error::Error;
use stipplefs::format::{BLOCK_ENTRY_SIZE_VARIABLE, Codec, SectionKind, Superblock};
use stipplefs::read::Image;
use stipplefs::write::{Builder, Node, Options, Timestamps};

fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// Forty files of random content, the third of which `edit` may rewrite.
fn tree(variable_blocks: bool, chunk_average: u32, edit: Option<fn(&mut Vec<u8>)>) -> Vec<u8> {
    let mut b = Builder::new(Options {
        block_size: 64 * 1024,
        chunk_average,
        codecs: Codec::Stored.bit() | Codec::Zstd.bit(),
        level: 3,
        dictionary_size: 0,
        threads: 1,
        memory_limit: None,
        timestamps: Timestamps::Clamp,
        compress_metadata: false,
        variable_blocks,
    })
    .unwrap();
    for i in 0..40u64 {
        let mut content = noise(i + 1, 20_000);
        if i == 2
            && let Some(edit) = edit
        {
            edit(&mut content);
        }
        b.insert(
            format!("f{i:02}").as_bytes(),
            Node::File {
                content,
                mode: 0o644,
                uid: 0,
                gid: 0,
                link_group: None,
            },
        )
        .unwrap();
    }
    b.build().unwrap()
}

/// Every block's stored bytes, as `(digest, stored length)`.
fn stored_blocks(image: &[u8]) -> Vec<(u64, u32)> {
    let img = Image::new(image.to_vec(), 0, image.len() as u64).unwrap();
    let index = img.block_index().unwrap();
    (0..index.count())
        .map(|i| {
            let e = index.get(i).unwrap();
            let at = (e.image_offset + e.stored_length as u64) as usize;
            let trailing = u64::from_le_bytes(image[at..at + 8].try_into().unwrap());
            if let Some(d) = e.digest {
                assert_eq!(d, trailing, "block {i}: index and trailing digests differ");
            }
            (trailing, e.stored_length)
        })
        .collect()
}

/// The share of `after`'s blocks whose stored bytes `before` already holds.
fn shared(before: &[u8], after: &[u8]) -> f64 {
    let old: HashSet<_> = stored_blocks(before).into_iter().collect();
    let new = stored_blocks(after);
    new.iter().filter(|b| old.contains(b)).count() as f64 / new.len() as f64
}

fn insert_early(content: &mut Vec<u8>) {
    content.splice(10_000..10_000, noise(99, 5_000));
}

#[test]
fn an_early_insertion_leaves_later_variable_blocks_intact() {
    let before = tree(true, 4096, None);
    let after = tree(true, 4096, Some(insert_early));
    let fraction = shared(&before, &after);
    assert!(
        fraction >= 0.8,
        "only {:.0}% of variable blocks survived an early insertion",
        fraction * 100.0
    );
}

#[test]
fn an_early_insertion_shifts_every_later_fixed_block() {
    let before = tree(false, 4096, None);
    let after = tree(false, 4096, Some(insert_early));
    let fraction = shared(&before, &after);
    assert!(
        fraction < 0.2,
        "{:.0}% of fixed blocks survived, so the variable test proves nothing",
        fraction * 100.0
    );
}

#[test]
fn large_chunks_still_end_blocks_where_the_content_says() {
    // Sixteen KiB chunks against 64 KiB blocks leave the cut mask its
    // narrowest. Ends must still follow content, not the first chunk past
    // the floor, or an early insertion shifts every block after it.
    let before = tree(true, 16384, None);
    let after = tree(true, 16384, Some(insert_early));
    let fraction = shared(&before, &after);
    assert!(
        fraction >= 0.5,
        "only {:.0}% of blocks survived an early insertion with large chunks",
        fraction * 100.0
    );
}

#[test]
fn variable_blocks_vary_and_stay_within_block_size() {
    let image = tree(true, 4096, None);
    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let index = img.block_index().unwrap();
    let lengths: Vec<u32> = (0..index.count())
        .map(|i| index.get(i).unwrap().decoded_length)
        .collect();
    assert!(lengths.iter().all(|&l| l > 0 && l <= 64 * 1024));
    let distinct: HashSet<_> = lengths.iter().collect();
    assert!(distinct.len() > lengths.len() / 2, "lengths: {lengths:?}");
    let mut at = 0;
    for (i, &l) in lengths.iter().enumerate() {
        assert_eq!(index.start(i as u32), Some(at));
        assert_eq!(index.locate(at), Some((i as u32, 0)));
        assert_eq!(
            index.locate(at + l as u64 - 1),
            Some((i as u32, l as u64 - 1))
        );
        at += l as u64;
    }
    assert_eq!(at, img.superblock().stream_length);
    assert_eq!(index.locate(at), None);
}

fn u64at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn reseal_superblock(image: &mut [u8]) {
    let d = xxh3(&image[..Superblock::CHECKSUM_OFFSET]);
    image[Superblock::CHECKSUM_OFFSET..4096].copy_from_slice(&d.to_le_bytes());
}

fn xxh3(bytes: &[u8]) -> u64 {
    twox_hash::XxHash3_64::oneshot(bytes)
}

/// Rewrite the block index with `edit`, then reseal its digest so only the
/// edit itself is wrong.
fn edit_block_index(image: &mut [u8], edit: impl Fn(&mut [u8])) {
    let base = 56 + SectionKind::Blocks as usize * 16;
    let offset = u64at(image, base) as usize;
    let size = u64at(image, base + 8) as usize;
    let body = offset..offset + size - 8;
    edit(&mut image[body.clone()]);
    let d = xxh3(&image[body.clone()]);
    image[body.end..body.end + 8].copy_from_slice(&d.to_le_bytes());
}

fn malformed(result: Result<impl Sized, Error>) -> &'static str {
    match result {
        Err(Error::Malformed { detail }) => detail,
        Err(other) => panic!("expected a malformed image, got {other}"),
        Ok(_) => panic!("a malformed image was accepted"),
    }
}

fn open(image: &[u8]) -> Result<(), Error> {
    let img = Image::new(image.to_vec(), 0, image.len() as u64)?;
    img.reader().map(|_| ())
}

#[test]
fn the_flag_is_refused_before_minor_version_four() {
    let mut image = tree(true, 4096, None);
    image[10..12].copy_from_slice(&3u16.to_le_bytes());
    reseal_superblock(&mut image);
    let detail = malformed(Image::new(image.clone(), 0, image.len() as u64));
    assert!(detail.contains("minor version 4"), "{detail}");
}

#[test]
fn an_empty_variable_block_is_refused() {
    let mut image = tree(true, 4096, None);
    edit_block_index(&mut image, |b| {
        b[12..16].copy_from_slice(&0u32.to_le_bytes())
    });
    let detail = malformed(open(&image));
    assert!(detail.contains("decodes to nothing"), "{detail}");
}

#[test]
fn decoded_lengths_must_add_up_to_the_stream() {
    let mut image = tree(true, 4096, None);
    edit_block_index(&mut image, |b| {
        let at = BLOCK_ENTRY_SIZE_VARIABLE + 12;
        let length = u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        b[at..at + 4].copy_from_slice(&(length - 1).to_le_bytes());
    });
    let detail = malformed(open(&image));
    assert!(detail.contains("do not add up"), "{detail}");
}

#[test]
fn an_index_digest_that_disagrees_with_the_block_is_refused() {
    let mut image = tree(true, 4096, None);
    edit_block_index(&mut image, |b| b[20] ^= 0xff);
    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let reader = img.reader().unwrap();
    let ino = reader.resolve(b"f00").unwrap();
    let mut buf = vec![0u8; 20_000];
    let detail = malformed(reader.read(ino, 0, &mut buf));
    assert!(detail.contains("disagrees"), "{detail}");
}
