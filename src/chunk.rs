//! Content-defined chunking.
//!
//! Boundaries are chosen by the content rather than by position, so inserting
//! or removing bytes shifts only the boundaries near the edit instead of every
//! boundary after it. That is what lets deduplication find shared regions
//! between files that are similar without being identical.
//!
//! The algorithm is FastCDC, from Xia et al., "FastCDC: a Fast and Efficient
//! Content-Defined Chunking Approach for Data Deduplication", USENIX ATC '16,
//! implemented from the paper.

/// Smallest chunk the chunker will emit, except for a final short chunk.
pub const MIN_DIVISOR: u32 = 4;

/// Largest chunk, as a multiple of the target average.
pub const MAX_MULTIPLIER: u32 = 8;

const fn gear_table() -> [u64; 256] {
    let mut t = [0u64; 256];
    let mut state: u64 = 0x2f39_1a4b_9c7d_5e83;
    let mut i = 0;
    while i < 256 {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        t[i] = z ^ (z >> 31);
        i += 1;
    }
    t
}

const GEAR: [u64; 256] = gear_table();

/// How the chunker splits content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkerConfig {
    /// Target average chunk size in bytes.
    pub average: u32,
    /// Smallest chunk, except for the last chunk of the input.
    pub min: u32,
    /// Largest chunk.
    pub max: u32,
    mask_strict: u64,
    mask_lax: u64,
}

impl ChunkerConfig {
    /// A configuration targeting an average chunk size, rounded up to a power
    /// of two.
    pub fn new(average: u32) -> ChunkerConfig {
        let average = average.next_power_of_two().max(64);
        let bits = average.trailing_zeros();
        ChunkerConfig {
            average,
            min: average / MIN_DIVISOR,
            max: average.saturating_mul(MAX_MULTIPLIER),
            mask_strict: mask(bits + 2),
            mask_lax: mask(bits.saturating_sub(2)),
        }
    }
}

fn mask(bits: u32) -> u64 {
    let bits = bits.min(63);
    (!0u64) << (64 - bits)
}

/// Length of the next chunk of `data`.
///
/// Below the target average a stricter mask is used and above it a laxer one,
/// which concentrates chunk sizes around the average instead of spreading them
/// across the whole permitted range.
pub fn next_boundary(data: &[u8], cfg: &ChunkerConfig) -> usize {
    let n = data.len();
    if n <= cfg.min as usize {
        return n;
    }
    let limit = n.min(cfg.max as usize);
    let normal = limit.min(cfg.average as usize);

    let mut fp: u64 = 0;
    let mut i = cfg.min as usize;
    while i < normal {
        fp = (fp << 1).wrapping_add(GEAR[data[i] as usize]);
        if fp & cfg.mask_strict == 0 {
            return i;
        }
        i += 1;
    }
    while i < limit {
        fp = (fp << 1).wrapping_add(GEAR[data[i] as usize]);
        if fp & cfg.mask_lax == 0 {
            return i;
        }
        i += 1;
    }
    limit
}

/// Split `data` into content-defined chunks.
pub fn chunks(data: &[u8], cfg: &ChunkerConfig) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < data.len() {
        let len = next_boundary(&data[at..], cfg);
        out.push((at, len));
        at += len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn chunks_cover_the_input_exactly_once() {
        let cfg = ChunkerConfig::new(4096);
        let data = corpus(400_000, 1);
        let cs = chunks(&data, &cfg);
        let mut at = 0;
        for (off, len) in &cs {
            assert_eq!(*off, at);
            at += len;
        }
        assert_eq!(at, data.len());
    }

    #[test]
    fn chunking_is_deterministic() {
        let cfg = ChunkerConfig::new(8192);
        let data = corpus(300_000, 7);
        assert_eq!(chunks(&data, &cfg), chunks(&data, &cfg));
    }

    #[test]
    fn chunk_sizes_stay_within_the_configured_bounds() {
        let cfg = ChunkerConfig::new(4096);
        let data = corpus(500_000, 3);
        let cs = chunks(&data, &cfg);
        for (i, (_, len)) in cs.iter().enumerate() {
            assert!(*len <= cfg.max as usize, "chunk {i} exceeds max");
            if i + 1 < cs.len() {
                assert!(*len >= cfg.min as usize, "chunk {i} below min");
            }
        }
    }

    #[test]
    fn the_average_is_near_the_target() {
        for target in [4096u32, 16384, 65536] {
            let cfg = ChunkerConfig::new(target);
            let data = corpus(4_000_000, 11);
            let cs = chunks(&data, &cfg);
            let mean = data.len() as f64 / cs.len() as f64;
            let ratio = mean / target as f64;
            assert!(
                (0.5..2.5).contains(&ratio),
                "target {target}: mean {mean:.0}, ratio {ratio:.2}"
            );
        }
    }

    #[test]
    fn an_insertion_shifts_only_nearby_boundaries() {
        let cfg = ChunkerConfig::new(4096);
        let data = corpus(400_000, 5);
        let before = chunks(&data, &cfg);

        let mut edited = data.clone();
        edited.splice(1000..1000, [0xAAu8; 7]);
        let after = chunks(&edited, &cfg);

        // Boundaries far past the edit should realign rather than all shift.
        let tail_before: Vec<usize> = before
            .iter()
            .map(|(o, l)| o + l)
            .filter(|&e| e > 100_000)
            .collect();
        let tail_after: Vec<usize> = after
            .iter()
            .map(|(o, l)| o + l - 7)
            .filter(|&e| e > 100_000)
            .collect();
        let shared = tail_before
            .iter()
            .filter(|e| tail_after.contains(e))
            .count();
        let ratio = shared as f64 / tail_before.len() as f64;
        assert!(
            ratio > 0.9,
            "only {:.0}% of later boundaries realigned after a 7 byte insertion",
            ratio * 100.0
        );
    }
}
