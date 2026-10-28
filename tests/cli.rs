//! End to end tests of the two command line tools.

#![cfg(all(feature = "cli", feature = "read", feature = "write"))]

use std::path::{Path, PathBuf};
use std::process::Command;

const MK: &str = env!("CARGO_BIN_EXE_mkstipple");
const ST: &str = env!("CARGO_BIN_EXE_stipple");

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "stipplefs-cli-{tag}-{}-{:?}",
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

fn make_tree(root: &Path) {
    std::fs::create_dir_all(root.join("nested/deeper")).unwrap();
    std::fs::write(root.join("readme.txt"), b"the same line. ".repeat(400)).unwrap();
    std::fs::write(
        root.join("nested/data.bin"),
        (0..40_000u32).map(|i| (i * 7) as u8).collect::<Vec<u8>>(),
    )
    .unwrap();
    std::fs::write(root.join("nested/deeper/small"), b"tiny").unwrap();
    std::fs::write(root.join("nested/deeper/empty"), b"").unwrap();
    std::fs::write(root.join("linked-a"), b"hard linked content. ".repeat(50)).unwrap();
    std::fs::hard_link(root.join("linked-a"), root.join("linked-b")).unwrap();
    std::os::unix::fs::symlink("nested/data.bin", root.join("pointer")).unwrap();
}

fn collect(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
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
            if meta.is_dir() {
                out.push((format!("d {rel}"), Vec::new()));
                stack.push(p);
            } else if meta.is_symlink() {
                let t = std::fs::read_link(&p).unwrap();
                out.push((format!("l {rel}"), t.to_string_lossy().as_bytes().to_vec()));
            } else {
                out.push((format!("f {rel}"), std::fs::read(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

fn build_image(src: &Path, out: &Path, extra: &[&str]) -> std::process::Output {
    let mut c = Command::new(MK);
    c.arg(src).arg(out).args([
        "--block-size",
        "4096",
        "--chunk-size",
        "4096",
        "--level",
        "3",
    ]);
    c.args(extra);
    c.output().unwrap()
}

#[test]
fn build_check_and_extract_reproduce_the_tree() {
    let s = Scratch::new("roundtrip");
    let src = s.path("src");
    make_tree(&src);
    let img = s.path("out.stipple");

    let built = build_image(&src, &img, &[]);
    assert!(
        built.status.success(),
        "mkstipple failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    assert!(img.exists());

    let checked = Command::new(ST).args(["check"]).arg(&img).output().unwrap();
    assert!(
        checked.status.success(),
        "check failed: {}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let report = String::from_utf8_lossy(&checked.stdout);
    assert!(
        report.contains("format          1.4"),
        "report was: {report}"
    );
    assert!(report.contains("verified"), "report was: {report}");

    let dest = s.path("out");
    let extracted = Command::new(ST)
        .args(["extract"])
        .arg(&img)
        .arg(&dest)
        .output()
        .unwrap();
    assert!(
        extracted.status.success(),
        "extract failed: {}",
        String::from_utf8_lossy(&extracted.stderr)
    );

    assert_eq!(
        collect(&src),
        collect(&dest),
        "extracted tree differs from the source"
    );
}

#[test]
fn a_subtree_can_be_extracted_alone() {
    let s = Scratch::new("subtree");
    let src = s.path("src");
    make_tree(&src);
    let img = s.path("out.stipple");
    assert!(build_image(&src, &img, &[]).status.success());

    let dest = s.path("out");
    let out = Command::new(ST)
        .args(["extract"])
        .arg(&img)
        .arg(&dest)
        .args(["--subtree", "nested/deeper"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(dest.join("nested/deeper/small").exists());
    assert!(
        !dest.join("readme.txt").exists(),
        "unrelated paths must not be written"
    );
    assert_eq!(
        std::fs::read(dest.join("nested/deeper/small")).unwrap(),
        b"tiny"
    );
}

#[test]
fn an_image_embedded_in_a_larger_file_is_readable() {
    let s = Scratch::new("embedded");
    let src = s.path("src");
    make_tree(&src);
    let img = s.path("out.stipple");
    assert!(build_image(&src, &img, &[]).status.success());

    let image = std::fs::read(&img).unwrap();
    let mut padded = vec![0x5Au8; 9999];
    let offset = padded.len() as u64;
    padded.extend_from_slice(&image);
    padded.extend_from_slice(&[0xA5u8; 1234]);
    let embedded = s.path("carrier.bin");
    std::fs::write(&embedded, &padded).unwrap();

    let dest = s.path("out");
    let out = Command::new(ST)
        .args(["extract"])
        .arg(&embedded)
        .arg(&dest)
        .args([
            "--offset",
            &offset.to_string(),
            "--length",
            &image.len().to_string(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(collect(&src), collect(&dest));
}

#[test]
fn an_explicit_cache_budget_is_accepted() {
    let s = Scratch::new("budget");
    let src = s.path("src");
    make_tree(&src);
    let img = s.path("out.stipple");
    assert!(build_image(&src, &img, &[]).status.success());

    let ok = Command::new(ST)
        .args(["check"])
        .arg(&img)
        .args(["--cache-budget", "65536"])
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );

    let too_small = Command::new(ST)
        .args(["check"])
        .arg(&img)
        .args(["--cache-budget", "64"])
        .output()
        .unwrap();
    assert!(
        !too_small.status.success(),
        "a budget below one block should fail"
    );
}

#[test]
fn failures_exit_non_zero_and_report_on_standard_error() {
    let s = Scratch::new("failure");

    let missing = Command::new(MK)
        .arg(s.path("nope"))
        .arg(s.path("out.stipple"))
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(!String::from_utf8_lossy(&missing.stderr).is_empty());
    assert!(
        String::from_utf8_lossy(&missing.stdout).is_empty(),
        "nothing should reach stdout"
    );
    assert!(
        !s.path("out.stipple").exists(),
        "no image should be left behind"
    );

    let src = s.path("src");
    make_tree(&src);
    let img = s.path("good.stipple");
    assert!(build_image(&src, &img, &[]).status.success());

    let mut corrupted = std::fs::read(&img).unwrap();
    // Corrupt a byte a digest actually covers. Trailing padding is not covered
    // by any digest, so flipping a byte there would not be detected.
    let at = {
        let opened =
            stipplefs::read::Image::new(corrupted.clone(), 0, corrupted.len() as u64).unwrap();
        opened.block_index().unwrap().get(0).unwrap().image_offset as usize
    };
    corrupted[at] ^= 0xff;
    let bad = s.path("bad.stipple");
    std::fs::write(&bad, &corrupted).unwrap();

    let out = Command::new(ST).args(["check"]).arg(&bad).output().unwrap();
    assert!(
        !out.status.success(),
        "check must fail on a corrupted image"
    );
    assert!(!String::from_utf8_lossy(&out.stderr).is_empty());
}

#[test]
fn no_partial_image_is_left_when_the_source_cannot_be_read() {
    let s = Scratch::new("partial");
    let out = s.path("out.stipple");
    let r = Command::new(MK)
        .arg(s.path("absent"))
        .arg(&out)
        .output()
        .unwrap();
    assert!(!r.status.success());
    assert!(!out.exists());
    assert!(
        !out.with_extension("stipple-partial").exists(),
        "a partial file was left behind"
    );
}

#[test]
fn the_builder_is_reproducible_from_the_command_line() {
    let s = Scratch::new("reproducible");
    let src = s.path("src");
    make_tree(&src);
    let a = s.path("a.stipple");
    let b = s.path("b.stipple");
    assert!(build_image(&src, &a, &["--threads", "1"]).status.success());
    assert!(build_image(&src, &b, &["--threads", "4"]).status.success());
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
}

#[test]
fn hard_links_survive_a_round_trip() {
    let s = Scratch::new("hardlink");
    let src = s.path("src");
    make_tree(&src);
    let img = s.path("out.stipple");
    assert!(build_image(&src, &img, &[]).status.success());

    let image = std::fs::read(&img).unwrap();
    let opened = stipplefs::read::Image::new(image.clone(), 0, image.len() as u64).unwrap();
    let r = opened.reader().unwrap();
    let root = r.inode(0).unwrap();
    let mut by_name = std::collections::BTreeMap::new();
    for i in 0..root.size as u32 {
        let e = r.dirent(root.payload as u32 + i).unwrap();
        by_name.insert(r.name(e).unwrap().to_vec(), e.inode);
    }
    let a = by_name[b"linked-a".as_slice()];
    let b = by_name[b"linked-b".as_slice()];
    assert_eq!(a, b, "hard links must share an inode through the tools");
    assert!(r.inode(a).unwrap().nlink >= 2);
}

#[test]
fn source_timestamps_do_not_affect_the_image_when_clamped() {
    let s = Scratch::new("timestamps");
    let src = s.path("src");
    make_tree(&src);

    let first = s.path("first.stipple");
    assert!(
        build_image(&src, &first, &["--mtime", "zero"])
            .status
            .success()
    );

    // Move every timestamp well into the past. The format records none of
    // them, so the bytes must not move either.
    let out = Command::new("find")
        .arg(&src)
        .args(["-exec", "touch", "-d", "2001-02-03 04:05:06", "{}", "+"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let second = s.path("second.stipple");
    assert!(
        build_image(&src, &second, &["--mtime", "zero"])
            .status
            .success()
    );

    assert_eq!(
        std::fs::read(&first).unwrap(),
        std::fs::read(&second).unwrap(),
        "changing source timestamps changed the image"
    );
}

#[test]
fn modification_times_survive_under_the_preserving_policy() {
    let s = Scratch::new("mtime-preserve");
    let src = s.path("src");
    make_tree(&src);

    // A time far enough in the past that it cannot be confused with now.
    let out = Command::new("find")
        .arg(&src)
        .args(["-exec", "touch", "-d", "2003-04-05 06:07:08", "{}", "+"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let img = s.path("out.stipple");
    assert!(
        build_image(&src, &img, &["--mtime", "preserve"])
            .status
            .success()
    );
    let dest = s.path("out");
    assert!(
        Command::new(ST)
            .args(["extract"])
            .arg(&img)
            .arg(&dest)
            .output()
            .unwrap()
            .status
            .success()
    );

    let stamp = |p: &std::path::Path| -> String {
        String::from_utf8_lossy(
            &Command::new("stat")
                .args(["-c", "%Y"])
                .arg(p)
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string()
    };
    assert_eq!(
        stamp(&src.join("readme.txt")),
        stamp(&dest.join("readme.txt")),
        "the modification time did not survive"
    );
}

#[test]
fn the_clamping_policy_ignores_source_timestamps() {
    let s = Scratch::new("mtime-clamp");
    let src = s.path("src");
    make_tree(&src);

    let first = s.path("first.stipple");
    assert!(
        build_image(&src, &first, &["--mtime", "zero"])
            .status
            .success()
    );

    assert!(
        Command::new("find")
            .arg(&src)
            .args(["-exec", "touch", "-d", "1999-01-02 03:04:05", "{}", "+"])
            .output()
            .unwrap()
            .status
            .success()
    );

    let second = s.path("second.stipple");
    assert!(
        build_image(&src, &second, &["--mtime", "zero"])
            .status
            .success()
    );
    assert_eq!(
        std::fs::read(&first).unwrap(),
        std::fs::read(&second).unwrap(),
        "clamping should make source timestamps irrelevant"
    );
}

#[test]
fn the_preserving_policy_is_still_reproducible() {
    let s = Scratch::new("mtime-repro");
    let src = s.path("src");
    make_tree(&src);
    let a = s.path("a.stipple");
    let b = s.path("b.stipple");
    assert!(
        build_image(&src, &a, &["--mtime", "preserve", "--threads", "1"])
            .status
            .success()
    );
    assert!(
        build_image(&src, &b, &["--mtime", "preserve", "--threads", "4"])
            .status
            .success()
    );
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
}

#[test]
fn the_build_help_names_the_defaults_and_what_reproducibility_costs() {
    let out = Command::new(MK).arg("--help").output().unwrap();
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("--mtime"),
        "the help does not mention the timestamp policy:\n{help}"
    );
    assert!(
        help.contains("[default: preserve]"),
        "the help does not name the timestamp default:\n{help}"
    );
    assert!(
        help.contains("byte-identical"),
        "the help does not say which policy makes two builds identical:\n{help}"
    );
    assert!(
        help.contains("--memory"),
        "the help does not mention the memory limit:\n{help}"
    );
    assert!(
        help.contains("Cannot go below one block"),
        "the help does not say what the memory limit cannot do:\n{help}"
    );
    assert!(
        help.contains("--no-bcj"),
        "the help does not mention the BCJ opt-out:\n{help}"
    );
    assert!(
        help.contains("--no-compress-metadata"),
        "the help does not mention the metadata opt-out:\n{help}"
    );
    assert!(
        help.contains("--order"),
        "the help does not mention stream ordering:\n{help}"
    );
}

#[test]
fn the_compression_opt_outs_change_the_image() {
    use stipplefs::format::{Codec, FLAG_COMPRESSED_METADATA};

    let s = Scratch::new("optouts");
    let src = s.path("src");
    std::fs::create_dir_all(&src).unwrap();
    // x86-shaped code: calls to one absolute target, so the BCJ filter turns
    let mut code = Vec::new();
    for p in (0..(256 << 10)).step_by(8) {
        let target = 0x1000i32;
        let rel = target - (p + 5);
        code.push(0xE8u8);
        code.extend_from_slice(&rel.to_le_bytes());
        code.extend_from_slice(&[0x90, 0x90, 0x90]);
    }
    std::fs::write(src.join("code.bin"), &code).unwrap();
    std::fs::write(src.join("text.txt"), b"the same line. ".repeat(400)).unwrap();
    let superblock = |path: &Path| {
        let img = stipplefs::read::Image::open(path).unwrap();
        *img.superblock()
    };
    // Total stored block bytes, which alignment padding does not hide.
    let stored_bytes = |path: &Path| -> u64 {
        let img = stipplefs::read::Image::open(path).unwrap();
        let index = img.block_index().unwrap();
        (0..index.count())
            .map(|i| index.get(i).unwrap().stored_length as u64)
            .sum()
    };
    let bcj_bits = Codec::ZstdBcj.bit() | Codec::ZstdDictBcj.bit();

    let plain = s.path("plain.stipple");
    let built = build_image(&src, &plain, &[]);
    assert!(
        built.status.success(),
        "default build failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let sb = superblock(&plain);
    assert_ne!(
        sb.codec_mask & bcj_bits,
        0,
        "no BCJ codec was chosen (mask {:#06x}, tree {:?})",
        sb.codec_mask,
        collect(&src)
            .iter()
            .map(|(n, b)| (n.clone(), b.len()))
            .collect::<Vec<_>>()
    );
    assert_ne!(sb.flags & FLAG_COMPRESSED_METADATA, 0);

    let nobcj = s.path("nobcj.stipple");
    assert!(
        build_image(&src, &nobcj, &["--no-bcj"]).status.success(),
        "--no-bcj build failed"
    );
    let sb = superblock(&nobcj);
    assert_eq!(
        sb.codec_mask & bcj_bits,
        0,
        "--no-bcj still emitted a BCJ codec"
    );
    assert!(
        stored_bytes(&plain) < stored_bytes(&nobcj),
        "BCJ did not shrink the stored blocks: {} against {}",
        stored_bytes(&plain),
        stored_bytes(&nobcj)
    );
    let nometa = s.path("nometa.stipple");
    assert!(
        build_image(&src, &nometa, &["--no-compress-metadata"])
            .status
            .success(),
        "--no-compress-metadata build failed"
    );
    assert_eq!(superblock(&nometa).flags & FLAG_COMPRESSED_METADATA, 0);

    let raw = s.path("raw.stipple");
    assert!(
        build_image(&src, &raw, &["--no-compress"]).status.success(),
        "--no-compress build failed"
    );
    let sb = superblock(&raw);
    assert_eq!(sb.codec_mask, Codec::Stored.bit());
    assert_eq!(sb.flags, 0, "--no-compress still compressed the metadata");

    let refused = build_image(
        &src,
        &s.path("refused.stipple"),
        &["--no-compress", "--no-bcj"],
    );
    assert!(
        !refused.status.success(),
        "--no-compress --no-bcj should be refused as redundant"
    );
}

#[test]
fn the_highest_level_does_more_than_the_ordinary_one() {
    let s = Scratch::new("levels");
    let src = s.path("src");
    make_tree(&src);
    let mut images = Vec::new();
    for level in ["19", "22"] {
        let out = s.path(&format!("l{level}.stipple"));
        let built = Command::new(MK)
            .arg(&src)
            .arg(&out)
            .args(["--block-size", "4096", "--level", level, "--mtime", "zero"])
            .output()
            .unwrap();
        assert!(built.status.success(), "building at level {level} failed");
        images.push(std::fs::read(&out).unwrap());
    }
    assert!(
        images[0] != images[1],
        "level 22 produced the same image as level 19, so the level is ignored"
    );
}

#[test]
fn a_level_outside_the_range_is_refused_by_name() {
    let s = Scratch::new("badlevel");
    let src = s.path("src");
    make_tree(&src);
    for level in ["0", "23", "100"] {
        let out = s.path(&format!("bad{level}.stipple"));
        let built = Command::new(MK)
            .arg(&src)
            .arg(&out)
            .args(["--block-size", "4096", "--level", level, "--mtime", "zero"])
            .output()
            .unwrap();
        assert!(
            !built.status.success(),
            "level {level} was accepted rather than refused"
        );
        let message = String::from_utf8_lossy(&built.stderr);
        assert!(
            message.contains("one to twenty two"),
            "the refusal did not name the accepted range: {message}"
        );
        assert!(
            !out.exists(),
            "a refused build left an image behind for level {level}"
        );
    }
}

#[test]
fn a_chunk_size_outside_the_range_is_refused_by_name() {
    let s = Scratch::new("badchunk");
    let src = s.path("src");
    make_tree(&src);
    let build = |size: &str| {
        let out = s.path(&format!("chunk{size}.stipple"));
        let built = Command::new(MK)
            .arg(&src)
            .arg(&out)
            .args(["--chunk-size", size, "--mtime", "zero"])
            .output()
            .unwrap();
        (built, out)
    };
    for size in ["0", "63", "268435457"] {
        let (built, out) = build(size);
        assert!(
            !built.status.success(),
            "chunk size {size} was accepted rather than refused"
        );
        let message = String::from_utf8_lossy(&built.stderr);
        assert!(
            message.contains("64 bytes to 256 MiB"),
            "the refusal did not name the accepted range: {message}"
        );
        assert!(!out.exists(), "a refused build left an image behind");
    }
    for size in ["64", "268435456"] {
        let (built, _) = build(size);
        assert!(built.status.success(), "chunk size {size} was refused");
    }
}
