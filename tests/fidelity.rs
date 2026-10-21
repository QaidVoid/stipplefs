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

/// An image holding one of every kind the format defines.
///
/// Built through the writer rather than from a tree, so that the kinds a test
/// machine may not let us create on disk are still covered.
fn every_kind_image() -> Vec<u8> {
    use stipplefs::write::{Builder, DeviceKind, Node, Options, SpecialKind};

    let mut b = Builder::new(Options {
        block_size: 4096,
        level: 3,
        ..Options::default()
    })
    .unwrap();
    b.insert(
        b"dir",
        Node::Directory {
            mode: 0o755,
            uid: 0,
            gid: 0,
        },
    )
    .unwrap();
    b.insert(
        b"file",
        Node::File {
            content: b"hello".to_vec(),
            mode: 0o644,
            uid: 0,
            gid: 0,
            link_group: None,
        },
    )
    .unwrap();
    b.insert(
        b"link",
        Node::Symlink {
            target: b"file".to_vec(),
            uid: 0,
            gid: 0,
        },
    )
    .unwrap();
    for (name, kind) in [
        (&b"pipe"[..], SpecialKind::Fifo),
        (b"sock", SpecialKind::Socket),
    ] {
        b.insert(
            name,
            Node::Special {
                kind,
                mode: 0o644,
                uid: 0,
                gid: 0,
            },
        )
        .unwrap();
    }
    for (name, kind, rdev) in [
        (&b"chr"[..], DeviceKind::Char, 0x0103u64),
        (b"blk", DeviceKind::Block, 0x0800),
    ] {
        b.insert(
            name,
            Node::Device {
                kind,
                rdev,
                mode: 0o600,
                uid: 0,
                gid: 0,
            },
        )
        .unwrap();
    }
    b.build().unwrap()
}

#[test]
fn every_kind_reads_back_as_itself() {
    use stipplefs::format::FileType;
    use stipplefs::read::Image;

    let image = every_kind_image();
    let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let r = img.reader().unwrap();
    let root = r.root();
    for (name, want) in [
        (&b"dir"[..], FileType::Directory),
        (b"file", FileType::Regular),
        (b"link", FileType::Symlink),
        (b"pipe", FileType::Fifo),
        (b"sock", FileType::Socket),
        (b"chr", FileType::CharDevice),
        (b"blk", FileType::BlockDevice),
    ] {
        let shown = String::from_utf8_lossy(name).into_owned();
        let ino = r
            .lookup(root, name)
            .unwrap_or_else(|| panic!("{shown} is missing"));
        let inode = r
            .inode(ino)
            .unwrap_or_else(|| panic!("{shown} has no inode"));
        assert_eq!(inode.file_type(), Some(want), "{shown} read back wrong");
    }

    for (name, want) in [(&b"chr"[..], 0x0103u64), (b"blk", 0x0800)] {
        let shown = String::from_utf8_lossy(name).into_owned();
        let ino = r.lookup(root, name).unwrap();
        assert_eq!(
            r.inode(ino).unwrap().device(),
            Some(want),
            "{shown} lost its device number"
        );
    }
    let file = r.lookup(root, b"file").unwrap();
    assert_eq!(
        r.inode(file).unwrap().device(),
        None,
        "a regular file reported a device number"
    );
}

/// Parse the `label count` lines a build prints after its summary.
fn reported_counts(stderr: &str) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for line in stderr.lines() {
        let trimmed = line.trim_start();
        if line == trimmed {
            continue;
        }
        if let Some((label, n)) = trimmed.rsplit_once(char::is_whitespace)
            && let Ok(n) = n.trim().parse::<u64>()
        {
            out.insert(label.trim().to_string(), n);
        }
    }
    out
}

#[test]
fn a_build_reports_the_counts_the_source_tree_holds() {
    let s = Scratch::new("counts");
    let src = s.path("src");
    let expected = build_fixture(&src);

    let built = Command::new(MK)
        .arg(&src)
        .arg(s.path("out.stipple"))
        .args(["--block-size", "4096", "--level", "3"])
        .output()
        .unwrap();
    assert!(built.status.success());
    let reported = reported_counts(&String::from_utf8_lossy(&built.stderr));

    let mut want: BTreeMap<&str, u64> = BTreeMap::new();
    for kind in expected.values() {
        let label = match kind.as_str() {
            "dir" => "directories",
            "file" => "files",
            "symlink" => "symlinks",
            "fifo" => "pipes",
            "socket" => "sockets",
            "char" => "char devices",
            "block" => "block devices",
            other => panic!("unexpected kind {other}"),
        };
        *want.entry(label).or_default() += 1;
    }
    // The root is in the image but is not one of the entries under it.
    *want.entry("directories").or_default() += 1;

    for (label, n) in &want {
        assert_eq!(
            reported.get(*label).copied(),
            Some(*n),
            "the build reported {:?} for {label}, the tree holds {n}",
            reported.get(*label)
        );
    }
}

#[test]
fn inspecting_an_image_of_every_kind_counts_each_one() {
    let s = Scratch::new("inspect");
    let image = s.path("all.stipple");
    std::fs::write(&image, every_kind_image()).unwrap();

    let out = Command::new(ST).arg("check").arg(&image).output().unwrap();
    assert!(
        out.status.success(),
        "check failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    for label in [
        "directories",
        "files",
        "symlinks",
        "pipes",
        "sockets",
        "char devices",
        "block devices",
    ] {
        let line = text
            .lines()
            .find(|l| l.starts_with(label))
            .unwrap_or_else(|| panic!("check reported no line for {label}:\n{text}"));
        let n: u64 = line[label.len()..]
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("could not read a count from {line:?}"));
        assert!(n > 0, "check reported {n} for {label}");
    }
    let per_inode = text
        .lines()
        .find(|l| l.contains("per inode"))
        .unwrap_or_else(|| panic!("check reported no metadata cost:\n{text}"));
    assert!(
        !per_inode.contains("0.0 per inode"),
        "metadata cost per inode was reported as zero: {per_inode}"
    );
}

#[test]
fn extracting_a_device_it_cannot_create_says_so_and_substitutes_nothing() {
    use std::os::unix::fs::FileTypeExt;

    let s = Scratch::new("mknod");
    let image = s.path("all.stipple");
    std::fs::write(&image, every_kind_image()).unwrap();
    let dest = s.path("out");

    let out = Command::new(ST)
        .arg("extract")
        .arg(&image)
        .arg(&dest)
        .output()
        .unwrap();

    let chr = dest.join("chr");
    if out.status.success() {
        // Running with the privilege to make device nodes, so every kind
        // should have been recreated as itself.
        let meta = std::fs::symlink_metadata(&chr).unwrap();
        assert!(
            meta.file_type().is_char_device(),
            "chr came back as something else"
        );
        let blk = std::fs::symlink_metadata(dest.join("blk")).unwrap();
        assert!(
            blk.file_type().is_block_device(),
            "blk came back as something else"
        );
        return;
    }

    let message = String::from_utf8_lossy(&out.stderr);
    assert!(
        message.contains("chr") || message.contains("blk"),
        "the failure did not name the entry it could not create: {message}"
    );
    assert!(
        message.contains("creating this entry failed"),
        "the failure did not give a reason: {message}"
    );
    if let Ok(meta) = std::fs::symlink_metadata(&chr) {
        assert!(
            !meta.file_type().is_file(),
            "a regular file was left where a device node belonged"
        );
    }
}

#[test]
fn extraction_recreates_every_kind_it_is_allowed_to() {
    use std::os::unix::fs::FileTypeExt;

    let s = Scratch::new("recreate");
    let src = s.path("src");
    let expected = build_fixture(&src);
    let image = s.path("out.stipple");
    let built = Command::new(MK)
        .arg(&src)
        .arg(&image)
        .args(["--block-size", "4096", "--level", "3"])
        .output()
        .unwrap();
    assert!(built.status.success());

    let dest = s.path("out");
    let out = Command::new(ST)
        .arg("extract")
        .arg(&image)
        .arg(&dest)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "extract failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    for (rel, kind) in &expected {
        let meta = std::fs::symlink_metadata(dest.join(rel))
            .unwrap_or_else(|e| panic!("{rel} was not extracted: {e}"));
        let t = meta.file_type();
        let got = if t.is_dir() {
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
        assert_eq!(got, kind, "{rel} came back as {got}");
    }
}

/// Mode, modification time and inode of every entry under `root`, keyed by
/// the raw bytes of its path, with inodes renumbered in order of appearance so
/// two trees can be compared.
fn shape(root: &Path) -> BTreeMap<Vec<u8>, (u32, Option<u64>, usize)> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    fn visit(
        dir: &Path,
        rel: &[u8],
        inodes: &mut Vec<u64>,
        out: &mut BTreeMap<Vec<u8>, (u32, Option<u64>, usize)>,
    ) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let mut key = rel.to_vec();
            key.push(b'/');
            key.extend_from_slice(path.file_name().unwrap().as_bytes());
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let id = match inodes.iter().position(|&i| i == meta.ino()) {
                Some(id) => id,
                None => {
                    inodes.push(meta.ino());
                    inodes.len() - 1
                }
            };
            let is_link = meta.file_type().is_symlink();
            let mode = if is_link { 0 } else { meta.mode() & 0o7777 };
            let mtime = (!is_link).then_some(meta.mtime() as u64);
            out.insert(key.clone(), (mode, mtime, id));
            if meta.is_dir() {
                visit(&path, &key, inodes, out);
            }
        }
    }

    let mut out = BTreeMap::new();
    visit(root, b"", &mut Vec::new(), &mut out);
    out
}

#[test]
fn extraction_restores_modes_times_hard_links_and_raw_names() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    let s = Scratch::new("restore");
    let src = s.path("src");
    let set = |p: &Path, mode: u32| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap()
    };
    let touch = |p: &Path, secs: u64| {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        std::fs::File::open(p).unwrap().set_modified(t).unwrap();
    };

    std::fs::create_dir_all(src.join("bin")).unwrap();
    std::fs::create_dir_all(src.join("locked")).unwrap();
    std::fs::write(src.join("bin/tool"), b"#!/bin/sh\n").unwrap();
    std::fs::write(src.join("secret"), b"s").unwrap();
    std::fs::write(src.join("locked/inside"), b"i").unwrap();
    std::fs::write(src.join("shared"), b"one inode, two names").unwrap();
    std::fs::hard_link(src.join("shared"), src.join("bin/also-shared")).unwrap();
    let raw = src.join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
    std::fs::write(&raw, b"latin-1 name").unwrap();
    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(b"caf\xe9"), src.join("to-raw"))
        .unwrap();

    set(&src.join("bin/tool"), 0o775);
    set(&src.join("secret"), 0o600);
    set(&src.join("locked/inside"), 0o444);
    set(&src.join("shared"), 0o640);
    for (i, p) in ["bin/tool", "secret", "locked/inside", "shared"]
        .iter()
        .enumerate()
    {
        touch(&src.join(p), 1_600_000_000 + i as u64);
    }
    touch(&raw, 1_600_000_100);
    touch(&src.join("bin"), 1_500_000_000);
    touch(&src.join("locked"), 1_500_000_001);
    set(&src.join("locked"), 0o555);

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
    let before = shape(&src);
    let after = shape(&dest);
    set(&src.join("locked"), 0o755);
    if dest.join("locked").exists() {
        set(&dest.join("locked"), 0o755);
    }
    assert!(
        extracted.status.success(),
        "extract failed: {}",
        String::from_utf8_lossy(&extracted.stderr)
    );
    assert_eq!(
        std::fs::read_link(dest.join("to-raw"))
            .unwrap()
            .as_os_str()
            .as_bytes(),
        b"caf\xe9"
    );
    assert_eq!(before, after, "the extracted tree differs from the source");
}
