//! How the reader behaves on images built with large blocks.
//!
//! A large block amortises compression over more content and costs more to
//! decode for one small read. What it must not do is cost more than decoding:
//! a block wanted twice while it is still cached must be decoded once.

#![cfg(all(
    feature = "read",
    feature = "write",
    feature = "zstd",
    feature = "metrics"
))]

use stipplefs::read::Image;
use stipplefs::write::{Builder, Node, Options};

/// Content that actually compresses, so blocks must be decoded rather than
/// stored and lent straight from the source.
fn pattern(n: usize, seed: u64) -> Vec<u8> {
    let body = format!("a line of mostly repeating text, batch {seed}. ");
    body.as_bytes().iter().copied().cycle().take(n).collect()
}

/// A tree whose content spans several blocks of `block_size`.
fn image_with_blocks(block_size: u32, files: usize, each: usize) -> Vec<u8> {
    let opts = Options {
        block_size,
        chunk_average: 65536,
        level: 1,
        threads: 1,
        ..Options::default()
    };
    let mut b = Builder::new(opts).unwrap();
    for i in 0..files {
        b.insert(
            format!("f{i:03}.bin").as_bytes(),
            Node::File {
                content: pattern(each, i as u64 + 1),
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

#[test]
fn a_block_is_decoded_once_while_it_stays_cached() {
    let block = 1 << 22; // 4 MiB
    let image = image_with_blocks(block, 24, 1 << 20);
    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let blocks = img.block_index().unwrap().count();
    assert!(blocks >= 4, "the test needs several blocks, got {blocks}");

    // A cache with room for every block, so nothing is ever evicted.
    let budget = (blocks as usize + 2) * block as usize;
    let r = img.reader_with_budget(budget).unwrap();

    // Read every file twice, in an order that revisits blocks.
    for _ in 0..2 {
        for ino in 1..=24u32 {
            let want = r.inode(ino).unwrap().size as usize;
            let mut buf = vec![0u8; want];
            r.read(ino, 0, &mut buf).unwrap();
        }
    }

    let m = r.metrics();
    use std::sync::atomic::Ordering::Relaxed;
    let decoded = m.cache_misses.load(Relaxed);
    let fetches = m.block_fetches.load(Relaxed);
    assert!(
        fetches > decoded,
        "the test should revisit blocks: {fetches} fetches, {decoded} decodes"
    );
    assert_eq!(
        decoded, blocks as u64,
        "every block should be decoded exactly once when the cache holds them all: \
         {decoded} decodes for {blocks} blocks"
    );
}

#[test]
fn a_cache_too_small_for_the_working_set_decodes_repeatedly() {
    // The counterpart: with room for one block, revisiting evicts and redecodes.
    let block = 1 << 22;
    let image = image_with_blocks(block, 24, 1 << 20);
    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let blocks = img.block_index().unwrap().count();
    let r = img.reader_with_budget(block as usize).unwrap();

    for _ in 0..2 {
        for ino in 1..=24u32 {
            let want = r.inode(ino).unwrap().size as usize;
            let mut buf = vec![0u8; want];
            r.read(ino, 0, &mut buf).unwrap();
        }
    }
    use std::sync::atomic::Ordering::Relaxed;
    let decoded = r.metrics().cache_misses.load(Relaxed);
    assert!(
        decoded > blocks as u64,
        "a budget below the working set should force redecoding, got {decoded} \
         decodes for {blocks} blocks"
    );
}
