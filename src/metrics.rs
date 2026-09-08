//! Counters for finding out where read time goes.
//!
//! Everything here is behind the `metrics` feature so that a normal build
//! carries no counters at all. The timings are coarse: they charge whole
//! phases rather than sampling, so they say which phase dominates rather than
//! giving an absolute cost.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Where time and bytes go while serving reads.
#[derive(Default, Debug)]
pub struct Metrics {
    /// Calls to the reader's `read`.
    pub reads: AtomicU64,
    /// Bytes returned to callers.
    pub read_bytes: AtomicU64,
    /// Times a block was needed.
    pub block_fetches: AtomicU64,
    /// Blocks borrowed from the source without any copy or decode.
    pub lent: AtomicU64,
    /// Blocks already present in the cache.
    pub cache_hits: AtomicU64,
    /// Blocks that had to be fetched and decoded.
    pub cache_misses: AtomicU64,
    /// Calls to the byte source.
    pub source_calls: AtomicU64,
    /// Bytes pulled from the byte source.
    pub source_bytes: AtomicU64,
    /// Nanoseconds inside the byte source.
    pub source_ns: AtomicU64,
    /// Nanoseconds decoding blocks.
    pub decode_ns: AtomicU64,
    /// Nanoseconds building a decompression context, inside `decode_ns`.
    pub dctx_ns: AtomicU64,
    /// Nanoseconds verifying block digests.
    pub digest_ns: AtomicU64,
    /// Nanoseconds copying decoded bytes into the caller's buffer.
    pub copy_ns: AtomicU64,
    /// Nanoseconds resolving an offset to an extent and block.
    pub resolve_ns: AtomicU64,
}

impl Metrics {
    /// Add one to a counter.
    pub fn bump(&self, field: &AtomicU64) {
        field.fetch_add(1, Ordering::Relaxed);
    }

    /// Add to a counter.
    pub fn add(&self, field: &AtomicU64, n: u64) {
        field.fetch_add(n, Ordering::Relaxed);
    }

    /// Time a closure and charge it to `field`.
    pub fn timed<T>(&self, field: &AtomicU64, f: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let out = f();
        field.fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        out
    }

    /// A one line per counter report.
    pub fn report(&self) -> String {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let (reads, fetches) = (g(&self.reads).max(1), g(&self.block_fetches).max(1));
        let phases = [
            ("resolve", g(&self.resolve_ns)),
            ("source", g(&self.source_ns)),
            ("digest", g(&self.digest_ns)),
            ("decode", g(&self.decode_ns)),
            ("copy", g(&self.copy_ns)),
        ];
        let total: u64 = phases.iter().map(|(_, n)| *n).sum::<u64>().max(1);

        let mut out = String::new();
        out.push_str(&format!(
            "reads          {:>12}\nread bytes     {:>12}\nblock fetches  {:>12}  ({:.2} per read)\n",
            g(&self.reads),
            g(&self.read_bytes),
            g(&self.block_fetches),
            g(&self.block_fetches) as f64 / reads as f64,
        ));
        out.push_str(&format!(
            "  lent         {:>12}\n  cache hits   {:>12}  ({:.1}% of fetches)\n  cache misses {:>12}\n",
            g(&self.lent),
            g(&self.cache_hits),
            100.0 * g(&self.cache_hits) as f64 / fetches as f64,
            g(&self.cache_misses),
        ));
        out.push_str(&format!(
            "source calls   {:>12}  {} bytes ({:.0} per call)\n",
            g(&self.source_calls),
            g(&self.source_bytes),
            g(&self.source_bytes) as f64 / g(&self.source_calls).max(1) as f64,
        ));
        out.push_str(&format!(
            "  of which building a decompression context: {} ns ({:.1}% of decode)\n",
            g(&self.dctx_ns),
            100.0 * g(&self.dctx_ns) as f64 / g(&self.decode_ns).max(1) as f64,
        ));
        out.push_str("time by phase\n");
        for (name, ns) in phases {
            out.push_str(&format!(
                "  {name:<10} {:>12} ns  {:>5.1}%  {:>8.0} ns/read\n",
                ns,
                100.0 * ns as f64 / total as f64,
                ns as f64 / reads as f64,
            ));
        }
        out
    }
}
