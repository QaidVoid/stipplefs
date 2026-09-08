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
    std::fs::create_dir_all(point).unwrap();
    let child = Command::new(ST)
        .arg("mount")
        .arg(image)
        .arg(point)
        .args(["--threads", "4"])
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
