//! Round trips over a tree containing every kind of entry.
//!
//! The corpora used to develop this crate contain only regular files,
//! directories and symbolic links, which is why entries of other kinds were
//! being discarded without anyone noticing. These fixtures exist to contain
//! the kinds those corpora lack.

#![cfg(all(feature = "cli", feature = "read", feature = "write"))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const MK: &str = env!("CARGO_BIN_EXE_mkstipple");
const ST: &str = env!("CARGO_BIN_EXE_stipple");

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "stipplefs-fidelity-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn have(tool: &str) -> bool {
    Command::new(tool).arg("--help").output().is_ok()
}

/// Build a tree holding one of every kind this format must be able to record.
///
/// Returns the kinds actually created, so a test can assert the fixture is
/// what it claims rather than trusting it.
fn build_fixture(root: &Path) -> BTreeMap<String, String> {
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::write(root.join("regular.txt"), b"content\n").unwrap();
    std::fs::write(root.join("empty.txt"), b"").unwrap();
    std::fs::write(root.join("nested/linked"), b"shared\n").unwrap();
    std::fs::hard_link(root.join("nested/linked"), root.join("nested/linked2")).unwrap();
    std::os::unix::fs::symlink("regular.txt", root.join("link")).unwrap();

    if have("mkfifo") {
        let _ = Command::new("mkfifo").arg(root.join("pipe")).status();
    }
    // A socket needs no external tool.
    let _ = std::os::unix::net::UnixListener::bind(root.join("socket"));

    if have("setfattr") {
        let _ = Command::new("setfattr")
            .args(["-n", "user.stipplefs", "-v", "marker"])
            .arg(root.join("regular.txt"))
            .status();
    }

    kinds(root)
}

/// Every entry under `root`, as a relative path mapped to its kind.
fn kinds(root: &Path) -> BTreeMap<String, String> {
    use std::os::unix::fs::FileTypeExt;
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for p in entries {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            let meta = std::fs::symlink_metadata(&p).unwrap();
            let t = meta.file_type();
            let kind = if t.is_dir() {
                stack.push(p.clone());
                "dir"
            } else if t.is_symlink() {
                "symlink"
            } else if t.is_fifo() {
                "fifo"
            } else if t.is_socket() {
                "socket"
            } else if t.is_char_device() {
                "char"
            } else if t.is_block_device() {
                "block"
            } else {
                "file"
            };
            out.insert(rel, kind.to_string());
        }
    }
    out
}

#[test]
fn the_fixture_contains_the_kinds_the_other_corpora_lack() {
    let s = Scratch::new("fixture");
    let src = s.path("src");
    let present = build_fixture(&src);

    let have_kind = |k: &str| present.values().any(|v| v == k);
    assert!(have_kind("file"), "fixture needs a regular file");
    assert!(have_kind("dir"), "fixture needs a directory");
    assert!(have_kind("symlink"), "fixture needs a symbolic link");
    assert!(
        have_kind("fifo"),
        "fixture needs a named pipe; mkfifo is required to run this suite"
    );
    assert!(have_kind("socket"), "fixture needs a socket");

    let meta = std::fs::metadata(src.join("nested/linked")).unwrap();
    use std::os::unix::fs::MetadataExt;
    assert!(meta.nlink() >= 2, "fixture needs a hard link");
    assert_eq!(std::fs::metadata(src.join("empty.txt")).unwrap().len(), 0);
}

#[test]
fn the_existing_corpora_contain_no_special_entries() {
    // Recorded rather than assumed: this is why the loss went unnoticed.
    let mut checked = 0;
    for root in ["/usr/include"] {
        let p = Path::new(root);
        if !p.is_dir() {
            continue;
        }
        checked += 1;
        let out = Command::new("find")
            .arg(p)
            .args(["!", "-type", "f", "!", "-type", "d", "!", "-type", "l"])
            .output()
            .unwrap();
        let special = String::from_utf8_lossy(&out.stdout);
        let n = special.lines().filter(|l| !l.is_empty()).count();
        assert_eq!(
            n, 0,
            "{root} was expected to hold no special entries, found {n}"
        );
    }
    assert!(checked > 0, "no corpus was available to check");
}

#[test]
fn a_tree_of_every_kind_round_trips() {
    let s = Scratch::new("roundtrip");
    let src = s.path("src");
    let expected = build_fixture(&src);
    let image = s.path("out.stipple");

    let built = Command::new(MK)
        .arg(&src)
        .arg(&image)
        .args(["--block-size", "4096", "--level", "3"])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "mkstipple failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );

    let dest = s.path("out");
    let extracted = Command::new(ST)
        .args(["extract"])
        .arg(&image)
        .arg(&dest)
        .output()
        .unwrap();
    assert!(
        extracted.status.success(),
        "extract failed: {}",
        String::from_utf8_lossy(&extracted.stderr)
    );

    // Extended attributes must survive as well as kinds.
    if have("getfattr") {
        let dump = |p: &Path| -> String {
            let out = Command::new("getfattr")
                .args(["-d", "--absolute-names"])
                .arg(p)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| l.contains('='))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let before = dump(&src.join("regular.txt"));
        let after = dump(&dest.join("regular.txt"));
        assert!(
            before.contains("user.stipplefs"),
            "the fixture should carry an extended attribute, got: {before:?}"
        );
        assert_eq!(before, after, "extended attributes did not survive");
    }

    let got = kinds(&dest);
    let missing: Vec<&String> = expected.keys().filter(|k| !got.contains_key(*k)).collect();
    assert!(
        missing.is_empty(),
        "entries lost in the round trip: {missing:?}"
    );
    assert_eq!(expected, got, "entry kinds differ after a round trip");
}

/// Try to create a character device, reporting why if it is not possible.
///
/// Creating one needs a privilege an ordinary test run does not have, so this
/// returns `None` with a reason rather than failing. A skip that says nothing
/// is indistinguishable from a pass.
fn try_make_device(at: &Path) -> Result<(), String> {
    if !have("mknod") {
        return Err("mknod is not available".into());
    }
    let out = Command::new("mknod")
        .arg(at)
        .args(["c", "1", "3"])
        .output()
        .map_err(|e| format!("running mknod: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[test]
fn a_device_node_round_trips_where_one_can_be_created() {
    let s = Scratch::new("device");
    let src = s.path("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("regular.txt"), b"content\n").unwrap();

    if let Err(reason) = try_make_device(&src.join("nulldev")) {
        eprintln!(
            "skipping: a character device could not be created ({reason}); \
             run this suite with the privilege to create device nodes to cover \
             that path"
        );
        return;
    }

    let before = kinds(&src);
    assert_eq!(
        before.get("nulldev").map(String::as_str),
        Some("char"),
        "the fixture should hold a character device"
    );

    let image = s.path("out.stipple");
    let built = Command::new(MK)
        .arg(&src)
        .arg(&image)
        .args(["--block-size", "4096", "--level", "3"])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "mkstipple failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );

    let dest = s.path("out");
    let extracted = Command::new(ST)
        .args(["extract"])
        .arg(&image)
        .arg(&dest)
        .output()
        .unwrap();
    assert!(
        extracted.status.success(),
        "extract failed: {}",
        String::from_utf8_lossy(&extracted.stderr)
    );

    assert_eq!(before, kinds(&dest), "the device node did not survive");
}
