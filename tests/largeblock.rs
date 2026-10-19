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

#[test]
fn a_large_block_costs_little_beyond_decoding_it() {
    // The cache once copied every decoded block into a second allocation on
    // its way into an `Arc`. That copy is invisible next to a small block and
    // costs more than the decode itself on a large one, so what this pins down
    // is the cost the cache adds on top of the work that has to happen.
    use std::sync::atomic::Ordering::Relaxed;

    let block = 1 << 22; // 4 MiB
    let image = image_with_blocks(block, 64, 1 << 20);
    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let blocks = img.block_index().unwrap().count();
    let r = img
        .reader_with_budget((blocks as usize + 2) * block as usize)
        .unwrap();

    for ino in 1..=64u32 {
        let want = r.inode(ino).unwrap().size as usize;
        let mut buf = vec![0u8; want];
        r.read(ino, 0, &mut buf).unwrap();
    }

    let m = r.metrics();
    let decode = m.decode_ns.load(Relaxed);
    let necessary = decode + m.source_ns.load(Relaxed) + m.digest_ns.load(Relaxed);
    let overhead = m.fetch_ns.load(Relaxed).saturating_sub(necessary);
    assert!(decode > 0, "the test decoded nothing");
    assert!(
        overhead * 4 < decode,
        "fetching a block cost {overhead} ns beyond the {decode} ns of decoding it, \
         over {blocks} blocks of {} MiB",
        block >> 20
    );
}

#[test]
fn bulk_reading_costs_what_it_decodes_at_either_block_size() {
    use std::sync::atomic::Ordering::Relaxed;

    // The same content twice, differing only in how it is cut into blocks.
    // Given a cache able to hold the working set, a large block should cost no
    // more per decoded byte than a small one, and in fact less, since the
    // fixed cost of a block is spread over more bytes. A cost per byte that
    // rises with the block size would mean large blocks are paying for
    // something other than the volume they decode.
    // Timing on a loaded machine only ever comes out too high, so take the
    // best of a few passes rather than a single sample.
    let mut rates = Vec::new();
    for block in [1u32 << 16, 1 << 22] {
        let image = image_with_blocks(block, 64, 1 << 20);
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let blocks = img.block_index().unwrap().count() as usize;
        let mut best = f64::MAX;
        for _ in 0..3 {
            let r = img
                .reader_with_budget((blocks + 2) * block as usize)
                .unwrap();
            for ino in 1..=64u32 {
                let want = r.inode(ino).unwrap().size as usize;
                let mut buf = vec![0u8; want];
                r.read(ino, 0, &mut buf).unwrap();
            }
            let m = r.metrics();
            let decoded = m.cache_misses.load(Relaxed) * block as u64;
            assert!(decoded > 0, "nothing was decoded at block size {block}");
            best = best.min(m.fetch_ns.load(Relaxed) as f64 / decoded as f64);
        }
        rates.push((block, best));
    }
    let (small_block, small) = rates[0];
    let (large_block, large) = rates[1];
    assert!(
        large < small * 1.5,
        "a {large_block} byte block cost {large:.4} ns per decoded byte against \
         {small:.4} for a {small_block} byte block, so the per-byte cost grows \
         with the size of the block"
    );
}

/// Reads that walk a file from start to end, in `step` sized pieces.
#[cfg(feature = "metrics")]
fn stream_file(r: &stipplefs::read::Reader<'_>, ino: u32, step: usize) {
    let size = r.inode(ino).unwrap().size;
    let mut at = 0u64;
    let mut buf = vec![0u8; step];
    while at < size {
        let n = r.read(ino, at, &mut buf).unwrap();
        assert!(n > 0);
        at += n as u64;
    }
}

#[test]
fn a_streamed_file_is_recognised_and_kept_only_at_low_priority() {
    use std::sync::atomic::Ordering::Relaxed;

    let block = 1 << 16;
    let image = image_with_blocks(block, 8, 4 << 20);
    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let blocks = img.block_index().unwrap().count() as usize;
    // A budget with room for every block, so nothing forces an eviction.
    let r = img
        .reader_with_budget((blocks + 2) * block as usize)
        .unwrap();

    stream_file(&r, 1, 4096);

    let m = r.metrics();
    assert!(
        m.sequential_runs.load(Relaxed) > 0,
        "streaming a file start to end was not recognised as a run"
    );
    let streamed = m.streamed_blocks.load(Relaxed);
    assert!(
        streamed > 0,
        "a recognised run fetched no blocks without retaining them"
    );
    assert!(
        r.cache_used() < streamed as usize * block as usize,
        "the cache kept {} bytes of the {} it streamed through",
        r.cache_used(),
        streamed as usize * block as usize
    );
}

#[test]
fn scattered_reads_are_not_taken_for_a_run() {
    use std::sync::atomic::Ordering::Relaxed;

    let block = 1 << 16;
    let image = image_with_blocks(block, 24, 1 << 20);
    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let r = img.reader_with_budget(64 * block as usize).unwrap();

    let mut buf = vec![0u8; 4096];
    for round in 0..8u64 {
        for ino in 1..=24u32 {
            let size = r.inode(ino).unwrap().size;
            let at = (round * 7919 + ino as u64 * 104729) % (size - 4096);
            r.read(ino, at, &mut buf).unwrap();
        }
    }
    assert_eq!(
        r.metrics().sequential_runs.load(Relaxed),
        0,
        "reads at unrelated offsets were mistaken for a sequential run"
    );
}

#[test]
fn recognition_does_not_change_what_a_read_returns() {
    let block = 1 << 16;
    let image = image_with_blocks(block, 4, 1 << 20);
    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let r = img.reader_with_budget(64 * block as usize).unwrap();

    let size = r.inode(1).unwrap().size as usize;
    let mut forwards = vec![0u8; size];
    let mut at = 0usize;
    while at < size {
        let n = r.read(1, at as u64, &mut forwards[at..]).unwrap();
        at += n;
    }

    // The same bytes gathered back to front, which is never recognised.
    let mut backwards = vec![0u8; size];
    let step = 4096;
    let mut start = size.saturating_sub(step);
    loop {
        let end = (start + step).min(size);
        let mut piece = vec![0u8; end - start];
        let mut got = 0;
        while got < piece.len() {
            let n = r.read(1, (start + got) as u64, &mut piece[got..]).unwrap();
            got += n;
        }
        backwards[start..end].copy_from_slice(&piece);
        if start == 0 {
            break;
        }
        start = start.saturating_sub(step);
    }
    assert_eq!(forwards, backwards, "reading order changed the bytes");
}

#[test]
fn streaming_does_not_evict_a_working_set() {
    use std::sync::atomic::Ordering::Relaxed;

    // A budget that holds the working set with room to spare, but nowhere near
    // the large file streamed alongside it.
    let block = 1 << 16;
    let image = image_with_blocks(block, 40, 1 << 20);
    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let r = img.reader_with_budget(48 * block as usize).unwrap();

    let mut buf = vec![0u8; 4096];
    let mut working_set = || {
        for ino in 1..=8u32 {
            r.read(ino, 0, &mut buf).unwrap();
        }
    };

    working_set();
    let warm = r.metrics().cache_misses.load(Relaxed);
    working_set();
    let settled = r.metrics().cache_misses.load(Relaxed);
    assert_eq!(settled, warm, "the working set did not settle in the cache");

    stream_file(&r, 40, 4096);
    let streamed = r.metrics().cache_misses.load(Relaxed);

    working_set();
    let after = r.metrics().cache_misses.load(Relaxed);
    assert_eq!(
        after,
        streamed,
        "streaming a large file cost the working set {} decodes",
        after - streamed
    );
}
#[test]
fn the_default_cache_budget_grows_with_the_block_size() {
    // A budget of two blocks was a thrashing cliff; the default must hold
    // several blocks at every permitted block size, and never drop below the
    // small-block default.
    for block_size in [4096u32, 65536, 1 << 20, 16 << 20] {
        let image = image_with_blocks(block_size, 2, block_size as usize);
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let budget = img.default_budget();
        let expected = stipplefs::read::DEFAULT_CACHE_BUDGET
            .max(stipplefs::read::DEFAULT_BLOCKS_RETAINED * block_size as usize);
        assert_eq!(
            budget, expected,
            "block size {block_size} got budget {budget}, wanted {expected}"
        );
        assert!(
            budget >= 8 * block_size as usize,
            "block size {block_size} got a budget of only {budget} bytes"
        );
    }
}

#[test]
fn streaming_decodes_each_block_once() {
    use std::sync::atomic::Ordering::Relaxed;
    let block = 1 << 16;
    let image = image_with_blocks(block, 4, 4 << 20);
    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let r = img.reader_with_budget(64 * block as usize).unwrap();
    let size = r.inode(1).unwrap().size;
    let spans = size.div_ceil(block as u64);
    stream_file(&r, 1, 4096);
    let misses = r.metrics().cache_misses.load(Relaxed);
    assert!(
        misses <= spans + 2,
        "streaming a file spanning {spans} blocks decoded {misses} times"
    );
}

#[test]
fn a_sequential_pass_does_not_fill_a_large_cache() {
    // A budget nothing can exhaust still must not end up holding every block a
    // one-pass read went through, because none of them will be read again.
    let block = 1 << 16;
    let image = image_with_blocks(block, 200, 200 * 1024);
    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let blocks = img.block_index().unwrap().count() as usize;
    let r = img
        .reader_with_budget((blocks + 8) * block as usize)
        .unwrap();
    for ino in 1..=200u32 {
        stream_file(&r, ino, 4096);
    }
    let held = r.cache_used() / block as usize;
    assert!(
        held * 2 < blocks,
        "after streaming {blocks} blocks the cache still holds {held} of them"
    );
}
