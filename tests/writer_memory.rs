//! What the writer holds in memory while it builds an image.
//!
//! The writer keeps the logical stream and the encoded blocks in a scratch
//! file rather than in memory, and reads each source file only when its bytes
//! are placed. The property worth defending is that none of what it does hold
//! grows with the size of the tree being packed.

#![cfg(all(feature = "write", feature = "cli", feature = "fs", feature = "zstd"))]

use std::path::{Path, PathBuf};
use std::process::Command;

const MK: &str = env!("CARGO_BIN_EXE_mkstipple");

/// A directory that removes itself, so a failing test leaves nothing behind.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "stipplefs-memory-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A tree of `mib` mebibytes whose files share no content, so that
/// deduplication cannot quietly shrink the work.
fn tree(root: &Path, mib: usize) -> PathBuf {
    let dir = root.join(format!("t{mib}"));
    std::fs::create_dir_all(&dir).unwrap();
    const EACH: usize = 4 << 20;
    for i in 0..mib / 4 {
        let unit = format!("record {i:04} unique ");
        let mut body = unit.repeat(EACH / unit.len() + 2);
        body.truncate(EACH);
        std::fs::write(dir.join(format!("f{i:04}.txt")), body).unwrap();
    }
    dir
}

/// The peak resident set of one `mkstipple` run, in mebibytes.
///
/// `VmHWM` is the kernel's own high-water mark and only ever rises, so polling
/// it while the child runs and keeping the last reading gives its peak.
fn peak_rss_mib(args: &[&str]) -> u64 {
    let mut child = Command::new(MK)
        .args(args)
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let status_path = format!("/proc/{}/status", child.id());
    let mut peak = 0u64;
    loop {
        if let Ok(status) = std::fs::read_to_string(&status_path) {
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("VmHWM:")
                    && let Some(kb) = rest.split_whitespace().next()
                    && let Ok(kb) = kb.parse::<u64>()
                {
                    peak = peak.max(kb / 1024);
                }
            }
        }
        match child.try_wait().unwrap() {
            Some(status) => {
                assert!(status.success(), "mkstipple failed: {status}");
                return peak;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(2)),
        }
    }
}

#[test]
fn writer_memory_does_not_grow_with_the_size_of_the_tree() {
    let root = Scratch::new("growth");
    let mut seen = Vec::new();
    for mib in [64usize, 256] {
        let src = tree(root.path(), mib);
        let out = root.path().join(format!("i{mib}.stipple"));
        let peak = peak_rss_mib(&[
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            "--block-size",
            "8388608",
            "--threads",
            "4",
            "--memory",
            "33554432",
            "--no-compress",
            "--mtime",
            "zero",
        ]);
        seen.push((mib, peak));
        std::fs::remove_dir_all(&src).unwrap();
    }
    let (small_mib, small) = seen[0];
    let (large_mib, large) = seen[1];
    let tree_growth = (large_mib - small_mib) as u64;
    let memory_growth = large.saturating_sub(small);
    assert!(
        memory_growth * 4 < tree_growth,
        "peak memory tracked the tree: {small} MiB for {small_mib} MiB of tree, \
         {large} MiB for {large_mib} MiB"
    );
}

#[test]
fn the_image_does_not_depend_on_the_memory_limit() {
    let root = Scratch::new("limit");
    let src = tree(root.path(), 16);
    let mut images = Vec::new();
    for limit in ["1", "4194304", "1073741824"] {
        let out = root.path().join(format!("i{limit}.stipple"));
        let status = Command::new(MK)
            .args([
                src.to_str().unwrap(),
                out.to_str().unwrap(),
                "--block-size",
                "1048576",
                "--threads",
                "4",
                "--level",
                "3",
                "--memory",
                limit,
                "--mtime",
                "zero",
            ])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "building under limit {limit} failed");
        images.push((limit, std::fs::read(&out).unwrap()));
    }
    let (first_limit, first) = &images[0];
    for (limit, image) in &images[1..] {
        assert_eq!(
            first.len(),
            image.len(),
            "limit {first_limit} and limit {limit} produced different lengths"
        );
        assert!(
            first == image,
            "limit {first_limit} and limit {limit} produced different images"
        );
    }
}

#[test]
fn unusable_temporary_space_is_reported_by_location() {
    let root = Scratch::new("tmpdir");
    let src = tree(root.path(), 4);
    let out = root.path().join("out.stipple");
    let missing = root.path().join("no/such/place");
    let result = Command::new(MK)
        .args([
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            "--mtime",
            "zero",
        ])
        .env("TMPDIR", &missing)
        .output()
        .unwrap();
    assert!(!result.status.success(), "the build should have failed");
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(
        message.contains(missing.to_str().unwrap()),
        "the error should name the directory it tried to use: {message}"
    );
    assert!(
        !out.exists(),
        "a failed build left an image behind at {}",
        out.display()
    );
    assert!(
        !out.with_extension("stipple-partial").exists(),
        "a failed build left a partial image behind"
    );
}
