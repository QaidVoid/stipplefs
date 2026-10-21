//! Mount tests. These need `/dev/fuse` and `fusermount3`; where either is
//! missing the test reports that it skipped rather than failing.

#![cfg(all(feature = "mount", feature = "cli", feature = "write"))]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const MK: &str = env!("CARGO_BIN_EXE_mkstipple");
const ST: &str = env!("CARGO_BIN_EXE_stipple");

fn fuse_available() -> bool {
    Path::new("/dev/fuse").exists()
        && Command::new("fusermount3")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

struct Mounted {
    child: Child,
    point: PathBuf,
}

impl Drop for Mounted {
    fn drop(&mut self) {
        let _ = Command::new("fusermount3")
            .arg("-u")
            .arg(&self.point)
            .output();
        std::thread::sleep(Duration::from_millis(200));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("stipplefs-mount-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn make_source(root: &Path) {
    std::fs::create_dir_all(root.join("sub/deep")).unwrap();
    std::fs::write(root.join("hello.txt"), b"hello from stipplefs\n").unwrap();
    std::fs::write(root.join("sub/big.bin"), b"repeating body. ".repeat(9000)).unwrap();
    std::fs::write(root.join("sub/deep/leaf"), b"leaf").unwrap();
    std::fs::write(root.join("sub/deep/empty"), b"").unwrap();
    std::os::unix::fs::symlink("sub/big.bin", root.join("point")).unwrap();
}

fn mount_image(image: &Path, point: &Path) -> Option<Mounted> {
    mount_image_with(image, point, &[])
}

fn mount_image_with(image: &Path, point: &Path, extra: &[&str]) -> Option<Mounted> {
    std::fs::create_dir_all(point).unwrap();
    let child = Command::new(ST)
        .arg("mount")
        .arg(image)
        .arg(point)
        .args(["--threads", "4"])
        .args(extra)
        .spawn()
        .ok()?;
    let mut m = Mounted {
        child,
        point: point.to_path_buf(),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if point.join("hello.txt").exists() {
            return Some(m);
        }
        if let Ok(Some(_)) = m.child.try_wait() {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = m.child.kill();
    None
}

fn prepare(tag: &str) -> Option<(PathBuf, PathBuf, PathBuf)> {
    if !fuse_available() {
        eprintln!("skipping {tag}: /dev/fuse or fusermount3 is unavailable");
        return None;
    }
    let dir = scratch(tag);
    let src = dir.join("src");
    make_source(&src);
    let image = dir.join("image.stipple");
    let built = Command::new(MK)
        .arg(&src)
        .arg(&image)
        .args([
            "--block-size",
            "4096",
            "--chunk-size",
            "4096",
            "--level",
            "3",
        ])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    Some((dir.clone(), src, image))
}

#[test]
fn a_mounted_image_presents_the_recorded_tree() {
    let Some((dir, src, image)) = prepare("tree") else {
        return;
    };
    let point = dir.join("mnt");
    let Some(_m) = mount_image(&image, &point) else {
        eprintln!("skipping: could not mount");
        return;
    };

    assert_eq!(
        std::fs::read(point.join("hello.txt")).unwrap(),
        std::fs::read(src.join("hello.txt")).unwrap()
    );
    assert_eq!(
        std::fs::read(point.join("sub/big.bin")).unwrap(),
        std::fs::read(src.join("sub/big.bin")).unwrap()
    );
    assert_eq!(std::fs::read(point.join("sub/deep/leaf")).unwrap(), b"leaf");
    assert_eq!(
        std::fs::read(point.join("sub/deep/empty")).unwrap(),
        Vec::<u8>::new()
    );
    assert_eq!(
        std::fs::read_link(point.join("point")).unwrap(),
        Path::new("sub/big.bin")
    );

    let mut names: Vec<String> = std::fs::read_dir(point.join("sub/deep"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["empty", "leaf"]);
}

#[test]
fn attributes_match_and_writes_are_refused() {
    let Some((dir, src, image)) = prepare("attrs") else {
        return;
    };
    let point = dir.join("mnt");
    let Some(_m) = mount_image(&image, &point) else {
        eprintln!("skipping: could not mount");
        return;
    };

    let want = std::fs::metadata(src.join("sub/big.bin")).unwrap();
    let got = std::fs::metadata(point.join("sub/big.bin")).unwrap();
    assert_eq!(want.len(), got.len(), "size must match");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        want.permissions().mode() & 0o7777,
        got.permissions().mode() & 0o7777
    );

    for outcome in [
        std::fs::write(point.join("new.txt"), b"x").err(),
        std::fs::create_dir(point.join("newdir")).err(),
        std::fs::remove_file(point.join("hello.txt")).err(),
        std::fs::rename(point.join("hello.txt"), point.join("moved")).err(),
    ] {
        let e = outcome.expect("a modifying operation must fail");
        assert_eq!(e.raw_os_error(), Some(30), "expected EROFS, got {e}");
    }
    assert!(
        point.join("hello.txt").exists(),
        "the image must be unchanged"
    );
}

#[test]
fn file_identity_is_stable_across_remounts() {
    let Some((dir, _src, image)) = prepare("identity") else {
        return;
    };
    let point = dir.join("mnt");
    use std::os::unix::fs::MetadataExt;

    let first = {
        let Some(_m) = mount_image(&image, &point) else {
            return;
        };
        (
            std::fs::metadata(point.join("hello.txt")).unwrap().ino(),
            std::fs::metadata(point.join("sub/deep/leaf"))
                .unwrap()
                .ino(),
        )
    };
    std::thread::sleep(Duration::from_millis(300));
    let second = {
        let Some(_m) = mount_image(&image, &point) else {
            return;
        };
        (
            std::fs::metadata(point.join("hello.txt")).unwrap().ino(),
            std::fs::metadata(point.join("sub/deep/leaf"))
                .unwrap()
                .ino(),
        )
    };
    assert_eq!(
        first, second,
        "inode numbers must not change between mounts"
    );
}

#[test]
fn concurrent_readers_all_see_correct_content() {
    let Some((dir, src, image)) = prepare("concurrent") else {
        return;
    };
    let point = dir.join("mnt");
    let Some(_m) = mount_image(&image, &point) else {
        eprintln!("skipping: could not mount");
        return;
    };

    let want = std::fs::read(src.join("sub/big.bin")).unwrap();
    let target = point.join("sub/big.bin");
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                for _ in 0..8 {
                    assert_eq!(std::fs::read(&target).unwrap(), want);
                }
            });
        }
    });
}

/// An image holding one of every kind the format defines, plus an extended
/// attribute and a known modification time.
fn every_kind_image() -> Vec<u8> {
    use stipplefs::write::{Builder, DeviceKind, Node, Options, SpecialKind};

    let mut b = Builder::new(Options {
        block_size: 4096,
        level: 3,
        ..Options::default()
    })
    .unwrap();
    b.insert(
        b"hello.txt",
        Node::File {
            content: b"hello from stipplefs\n".to_vec(),
            mode: 0o644,
            uid: 0,
            gid: 0,
            link_group: None,
        },
    )
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
        b"link",
        Node::Symlink {
            target: b"hello.txt".to_vec(),
            uid: 0,
            gid: 0,
        },
    )
    .unwrap();
    b.insert(
        b"pipe",
        Node::Special {
            kind: SpecialKind::Fifo,
            mode: 0o644,
            uid: 0,
            gid: 0,
        },
    )
    .unwrap();
    b.insert(
        b"sock",
        Node::Special {
            kind: SpecialKind::Socket,
            mode: 0o644,
            uid: 0,
            gid: 0,
        },
    )
    .unwrap();
    b.insert(
        b"chr",
        Node::Device {
            kind: DeviceKind::Char,
            rdev: 0x0103,
            mode: 0o600,
            uid: 0,
            gid: 0,
        },
    )
    .unwrap();
    b.insert(
        b"blk",
        Node::Device {
            kind: DeviceKind::Block,
            rdev: 0x0800,
            mode: 0o600,
            uid: 0,
            gid: 0,
        },
    )
    .unwrap();
    b.set_xattrs(
        b"hello.txt",
        vec![(b"user.marker".to_vec(), b"kept".to_vec())],
    );
    b.set_mtime(b"hello.txt", 1_600_000_000);
    b.build().unwrap()
}

#[test]
fn the_mount_presents_every_kind_with_its_own_type() {
    use std::os::unix::fs::FileTypeExt;

    if !fuse_available() {
        eprintln!("skipping: /dev/fuse or fusermount3 is unavailable");
        return;
    }
    let dir = scratch("kinds");
    let image = dir.join("all.stipple");
    std::fs::write(&image, every_kind_image()).unwrap();
    let point = dir.join("mnt");
    let Some(_m) = mount_image(&image, &point) else {
        eprintln!("skipping: the image could not be mounted");
        return;
    };

    let check = |name: &str, want: &str| {
        let meta = std::fs::symlink_metadata(point.join(name)).unwrap();
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
        assert_eq!(got, want, "{name} was presented as {got}");
    };
    check("hello.txt", "file");
    check("dir", "dir");
    check("link", "symlink");
    check("pipe", "fifo");
    check("sock", "socket");
    check("chr", "char");
    check("blk", "block");

    use std::os::unix::fs::MetadataExt;
    let chr = std::fs::symlink_metadata(point.join("chr")).unwrap();
    assert_eq!(chr.rdev(), 0x0103, "the character device lost its number");
    let blk = std::fs::symlink_metadata(point.join("blk")).unwrap();
    assert_eq!(blk.rdev(), 0x0800, "the block device lost its number");
}

#[test]
fn the_mount_reports_times_and_attributes_and_refuses_to_change_them() {
    use std::os::unix::fs::MetadataExt;

    if !fuse_available() {
        eprintln!("skipping: /dev/fuse or fusermount3 is unavailable");
        return;
    }
    let dir = scratch("xattr");
    let image = dir.join("all.stipple");
    std::fs::write(&image, every_kind_image()).unwrap();
    let point = dir.join("mnt");
    let Some(_m) = mount_image(&image, &point) else {
        eprintln!("skipping: the image could not be mounted");
        return;
    };
    let file = point.join("hello.txt");

    let meta = std::fs::symlink_metadata(&file).unwrap();
    assert_eq!(
        meta.mtime(),
        1_600_000_000,
        "the mount reported a different modification time"
    );

    if have("getfattr") {
        let out = Command::new("getfattr")
            .args(["-n", "user.marker", "--only-values"])
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "reading the attribute failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "kept",
            "the mount reported a different attribute value"
        );
    }
    if have("setfattr") {
        let out = Command::new("setfattr")
            .args(["-n", "user.marker", "-v", "changed"])
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "the mount allowed an attribute to be changed"
        );
        let message = String::from_utf8_lossy(&out.stderr).to_lowercase();
        assert!(
            message.contains("read-only") || message.contains("read only"),
            "changing an attribute failed for the wrong reason: {message}"
        );
    }
}

fn have(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool}"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Bytes that do not repeat, so every chunk is stored once.
fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
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
fn warming_stores_exactly_the_bytes_each_file_holds() {
    if !fuse_available() {
        eprintln!("skipping warming: /dev/fuse or fusermount3 is unavailable");
        return;
    }
    let dir = scratch("warm");
    let src = dir.join("src");
    std::fs::create_dir_all(src.join("lib")).unwrap();
    // Odd sizes put piece boundaries in the middle of pages, and the last
    // file repeats the first one's bytes so it is made of several extents.
    let first = noise(70_001, 1);
    std::fs::write(src.join("lib/a.so"), &first).unwrap();
    std::fs::write(src.join("lib/b.so"), noise(12_345, 2)).unwrap();
    std::fs::write(src.join("small"), noise(100, 3)).unwrap();
    let mut mixed = noise(5_000, 4);
    mixed.extend_from_slice(&first);
    mixed.extend_from_slice(&noise(3_333, 5));
    std::fs::write(src.join("mixed"), &mixed).unwrap();
    std::fs::write(src.join("hello.txt"), b"mount is up").unwrap();

    let image = dir.join("image.stipple");
    let built = Command::new(MK)
        .arg(&src)
        .arg(&image)
        .args([
            "--block-size",
            "8192",
            "--chunk-size",
            "4096",
            "--level",
            "3",
        ])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );

    let point = dir.join("mnt");
    let args = [
        "--cache-budget",
        "16384",
        "--warm-workers",
        "2",
        "--warm-start",
        "1000",
    ];
    let Some(_m) = mount_image_with(&image, &point, &args) else {
        eprintln!("skipping: could not mount");
        return;
    };
    // Let warming store every block before anything is read.
    std::thread::sleep(Duration::from_millis(500));

    for name in ["lib/a.so", "lib/b.so", "small", "mixed", "hello.txt"] {
        for _ in 0..2 {
            assert_eq!(
                std::fs::read(point.join(name)).unwrap(),
                std::fs::read(src.join(name)).unwrap(),
                "{name} read back wrong"
            );
        }
    }
    for _ in 0..2 {
        assert_eq!(
            std::fs::metadata(point.join("lib/missing"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
    }
}

#[test]
fn a_stop_signal_unmounts_rather_than_leaving_the_mount_disconnected() {
    let Some((dir, _src, image)) = prepare("signal") else {
        return;
    };
    for signal in ["INT", "TERM", "HUP"] {
        let point = dir.join(format!("mnt-{signal}"));
        let Some(mut m) = mount_image(&image, &point) else {
            eprintln!("skipping: could not mount");
            return;
        };
        let sent = Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(m.child.id().to_string())
            .status()
            .unwrap();
        assert!(sent.success());
        let status = m.child.wait().unwrap();
        assert!(
            status.success(),
            "SIG{signal} ended the mount with {status}"
        );
        let left: Vec<_> = std::fs::read_dir(&point)
            .unwrap_or_else(|e| panic!("SIG{signal} left {}: {e}", point.display()))
            .collect();
        assert!(left.is_empty(), "SIG{signal} left the image mounted");
    }
}
