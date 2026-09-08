//! A bounded, single-flight cache of decoded blocks.
//!
//! Concurrent readers that want the same block wait for one decode rather
//! than each performing their own, and different blocks decode in parallel.

use crate::error::{Error, Result};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Default)]
struct Inner {
    ready: HashMap<u32, Arc<[u8]>>,
    recent: VecDeque<u32>,
    decoding: Vec<u32>,
    used: usize,
}

/// A cache of decoded blocks whose memory stays within a budget.
pub struct BlockCache {
    inner: Mutex<Inner>,
    changed: Condvar,
    budget: usize,
}

/// Clears the in-flight marker even if the decoder panics or returns early.
struct Flight<'c> {
    cache: &'c BlockCache,
    index: u32,
    done: bool,
}

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        if !self.done {
            let mut inner = match self.cache.inner.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            inner.decoding.retain(|&i| i != self.index);
            drop(inner);
            self.cache.changed.notify_all();
        }
    }
}

impl BlockCache {
    /// A cache holding at most `budget` bytes of decoded blocks.
    pub fn new(budget: usize) -> Self {
        BlockCache {
            inner: Mutex::new(Inner::default()),
            changed: Condvar::new(),
            budget,
        }
    }

    /// Bytes this cache is permitted to hold.
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Bytes currently held.
    pub fn used(&self) -> usize {
        self.lock().used
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    /// Fetch a block, decoding it with `decode` only if no other caller is
    /// already doing so.
    ///
    /// The lock is released while decoding, so different blocks proceed in
    /// parallel. A failed decode wakes every waiter and leaves no entry, so a
    /// later call may retry.
    pub fn get_or_decode<F>(&self, index: u32, decode: F) -> Result<Arc<[u8]>>
    where
        F: FnOnce() -> Result<Vec<u8>>,
    {
        let mut inner = self.lock();
        loop {
            if let Some(bytes) = inner.ready.get(&index) {
                let bytes = Arc::clone(bytes);
                touch(&mut inner, index);
                return Ok(bytes);
            }
            if inner.decoding.contains(&index) {
                inner = match self.changed.wait(inner) {
                    Ok(g) => g,
                    Err(p) => p.into_inner(),
                };
                continue;
            }
            inner.decoding.push(index);
            break;
        }
        drop(inner);

        let mut flight = Flight {
            cache: self,
            index,
            done: false,
        };
        let decoded = decode()?;
        let bytes: Arc<[u8]> = Arc::from(decoded);

        let mut inner = self.lock();
        inner.decoding.retain(|&i| i != index);
        flight.done = true;
        if bytes.len() <= self.budget {
            evict_for(&mut inner, bytes.len(), self.budget);
            if inner.ready.insert(index, Arc::clone(&bytes)).is_none() {
                inner.used += bytes.len();
                inner.recent.push_back(index);
            }
        }
        drop(inner);
        self.changed.notify_all();
        Ok(bytes)
    }
}

fn touch(inner: &mut Inner, index: u32) {
    if let Some(at) = inner.recent.iter().position(|&i| i == index) {
        inner.recent.remove(at);
    }
    inner.recent.push_back(index);
}

fn evict_for(inner: &mut Inner, incoming: usize, budget: usize) {
    while inner.used + incoming > budget {
        let Some(oldest) = inner.recent.pop_front() else {
            return;
        };
        if let Some(bytes) = inner.ready.remove(&oldest) {
            inner.used -= bytes.len();
        }
    }
}

/// Raised when a caller asks for a budget that cannot hold a single block.
pub(crate) fn check_budget(budget: usize, block_size: usize) -> Result<()> {
    if budget < block_size {
        return Err(Error::Malformed {
            detail: "cache budget is smaller than one block",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn a_hit_returns_the_same_bytes_without_decoding_again() {
        let cache = BlockCache::new(1 << 20);
        let calls = AtomicUsize::new(0);
        let decode = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![7u8; 64])
        };
        let a = cache.get_or_decode(0, decode).unwrap();
        let b = cache.get_or_decode(0, decode).unwrap();
        assert_eq!(a.as_ref(), b.as_ref());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn concurrent_readers_of_one_block_decode_it_once() {
        let cache = Arc::new(BlockCache::new(1 << 20));
        let calls = Arc::new(AtomicUsize::new(0));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));

        let mut handles = Vec::new();
        for _ in 0..16 {
            let cache = Arc::clone(&cache);
            let calls = Arc::clone(&calls);
            let entered = entered_tx.clone();
            let release = Arc::clone(&release_rx);
            handles.push(std::thread::spawn(move || {
                cache
                    .get_or_decode(0, || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        entered.send(()).unwrap();
                        let _ = release.lock().unwrap().recv_timeout(Duration::from_secs(5));
                        Ok(vec![9u8; 128])
                    })
                    .unwrap()
            }));
        }
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        release_tx.send(()).unwrap();
        for h in handles {
            assert_eq!(h.join().unwrap().len(), 128);
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "block decoded more than once"
        );
    }

    #[test]
    fn different_blocks_decode_concurrently() {
        let cache = Arc::new(BlockCache::new(1 << 20));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));

        let mut handles = Vec::new();
        for index in 0..2u32 {
            let cache = Arc::clone(&cache);
            let entered = entered_tx.clone();
            let release = Arc::clone(&release_rx);
            handles.push(std::thread::spawn(move || {
                cache
                    .get_or_decode(index, || {
                        entered.send(index).unwrap();
                        let _ = release.lock().unwrap().recv_timeout(Duration::from_secs(5));
                        Ok(vec![index as u8; 32])
                    })
                    .unwrap()
            }));
        }

        // Both decoders must be inside their closures at the same time. If the
        // cache serialised them, only one arrives and this times out.
        let a = entered_rx.recv_timeout(Duration::from_secs(5));
        let b = entered_rx.recv_timeout(Duration::from_secs(5));
        assert!(a.is_ok() && b.is_ok(), "decodes were serialised");
        release_tx.send(()).unwrap();
        release_tx.send(()).unwrap();
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn a_failed_decode_wakes_waiters_and_can_be_retried() {
        let cache = Arc::new(BlockCache::new(1 << 20));
        let r = cache.get_or_decode(3, || {
            Err(Error::Malformed {
                detail: "decode failed",
            })
        });
        assert!(r.is_err());
        assert_eq!(cache.used(), 0);
        let ok = cache.get_or_decode(3, || Ok(vec![1u8; 16])).unwrap();
        assert_eq!(ok.len(), 16);
    }

    #[test]
    fn a_panicking_decode_does_not_wedge_the_cache() {
        let cache = Arc::new(BlockCache::new(1 << 20));
        let c = Arc::clone(&cache);
        let panicked = std::thread::spawn(move || {
            c.get_or_decode(4, || -> Result<Vec<u8>> { panic!("decoder exploded") })
        })
        .join();
        assert!(panicked.is_err(), "the decode was expected to panic");

        let (tx, rx) = mpsc::channel();
        let c = Arc::clone(&cache);
        std::thread::spawn(move || {
            let r = c.get_or_decode(4, || Ok(vec![2u8; 8]));
            tx.send(r.map(|b| b.len())).unwrap();
        });
        let got = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("cache wedged after a panicking decode");
        assert_eq!(got.unwrap(), 8);
    }

    #[test]
    fn memory_stays_within_the_budget() {
        let cache = BlockCache::new(4096);
        for i in 0..64u32 {
            cache.get_or_decode(i, || Ok(vec![0u8; 1024])).unwrap();
            assert!(
                cache.used() <= cache.budget(),
                "used {} exceeds budget {}",
                cache.used(),
                cache.budget()
            );
        }
        assert!(cache.used() <= 4096);
    }

    #[test]
    fn one_budget_is_shared_by_concurrent_readers() {
        let cache = Arc::new(BlockCache::new(8192));
        let mut handles = Vec::new();
        for t in 0..8u32 {
            let cache = Arc::clone(&cache);
            handles.push(std::thread::spawn(move || {
                for i in 0..32u32 {
                    cache
                        .get_or_decode(t * 32 + i, || Ok(vec![0u8; 1024]))
                        .unwrap();
                    assert!(cache.used() <= cache.budget());
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert!(cache.used() <= 8192);
    }

    #[test]
    fn a_block_larger_than_the_budget_is_served_but_not_retained() {
        let cache = BlockCache::new(1024);
        let b = cache.get_or_decode(0, || Ok(vec![5u8; 4096])).unwrap();
        assert_eq!(b.len(), 4096);
        assert_eq!(cache.used(), 0);
    }
}
