//! Reads stipplefs images: inspection, extraction and mounting.

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use stipplefs::format::FileType;
use stipplefs::read::{Image, Reader};

#[derive(Parser)]
#[command(
    name = "stipple",
    about = "Inspect, extract and mount stipplefs images"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Report an image's structure and verify its integrity.
    Check(Located),
    /// Mount an image on a directory.
    #[cfg(feature = "mount")]
    Mount {
        #[command(flatten)]
        image: Located,
        /// Directory to mount on.
        mountpoint: PathBuf,
        /// Let other users see the mount.
        #[arg(long)]
        allow_other: bool,
        /// Event loop threads serving the mount.
        #[arg(long, default_value_t = 4)]
        threads: usize,
    },
    /// Time random reads served directly by the library, without FUSE.
    Bench {
        #[command(flatten)]
        image: Located,
        /// Random reads to perform.
        #[arg(long, default_value_t = 2000)]
        reads: usize,
        /// Size of each read in bytes.
        #[arg(long, default_value_t = 4096)]
        read_size: usize,
        /// Seed, so a run can be repeated exactly.
        #[arg(long, default_value_t = 20260908)]
        seed: u64,
    },
    /// Write an image's contents to a directory.
    Extract {
        #[command(flatten)]
        image: Located,
        /// Directory to write into.
        destination: PathBuf,
        /// Extract only this path within the image.
        #[arg(long)]
        subtree: Option<String>,
    },
}

#[derive(clap::Args)]
struct Located {
    /// File holding the image.
    path: PathBuf,
    /// Byte offset of the image within the file.
    #[arg(long, default_value_t = 0)]
    offset: u64,
    /// Length of the image in bytes. Defaults to the rest of the file.
    #[arg(long)]
    length: Option<u64>,
    /// Bytes of decoded blocks the reader may retain. Defaults to a size
    /// suited to the image's block size.
    #[arg(long)]
    cache_budget: Option<usize>,
    /// Map the image instead of reading it, so blocks are taken in place.
    #[cfg(feature = "mmap")]
    #[arg(long)]
    mmap: bool,
}

impl Located {
    fn open(&self) -> Result<Image<'static>, String> {
        let size = std::fs::metadata(&self.path)
            .map_err(|e| format!("{}: {e}", self.path.display()))?
            .len();
        let length = self.length.unwrap_or(size.saturating_sub(self.offset));

        #[cfg(feature = "mmap")]
        if self.mmap {
            let file = std::fs::File::open(&self.path)
                .map_err(|e| format!("{}: {e}", self.path.display()))?;
            // SAFETY: the caller has asked for a mapping and is responsible for
            // the image not being modified while it is mounted.
            let source = unsafe { stipplefs::source::MmapSource::new(&file) }
                .map_err(|e| format!("{}: {e}", self.path.display()))?;
            return Image::new(source, self.offset, length)
                .map_err(|e| format!("{}: {e}", self.path.display()));
        }

        Image::open_at(&self.path, self.offset, length)
            .map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

/// Create a named pipe, socket or device node at `at`.
///
/// `std` offers no way to make any of these, so this calls `mknod` directly.
fn create_special(at: &Path, kind: FileType, mode: u32, rdev: u64) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;

    let type_bits = match kind {
        FileType::Fifo => libc::S_IFIFO,
        FileType::Socket => libc::S_IFSOCK,
        FileType::CharDevice => libc::S_IFCHR,
        FileType::BlockDevice => libc::S_IFBLK,
        _ => return Err(format!("{}: not a special entry", at.display())),
    };

    let mut raw: Vec<u8> = at.as_os_str().as_bytes().to_vec();
    raw.push(0);

    // SAFETY: `raw` is a nul-terminated path that outlives the call, and the
    // mode and device number are plain integers. `mknod` reports failure
    // through its return value, which is checked below.
    let rc = unsafe {
        libc::mknod(
            raw.as_ptr() as *const libc::c_char,
            type_bits | (mode & 0o7777),
            rdev as libc::dev_t,
        )
    };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    Err(format!(
        "{}: creating this entry failed: {err}",
        at.display()
    ))
}

/// Attach extended attributes to an entry that has just been extracted.
fn restore_xattrs(at: &Path, attrs: &[(&[u8], &[u8])]) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    if attrs.is_empty() {
        return Ok(());
    }
    let mut raw: Vec<u8> = at.as_os_str().as_bytes().to_vec();
    raw.push(0);
    for (name, value) in attrs {
        let mut key: Vec<u8> = name.to_vec();
        key.push(0);
        // SAFETY: both the path and the name are nul-terminated and outlive
        // the call, and the value pointer and length describe one slice.
        let rc = unsafe {
            libc::lsetxattr(
                raw.as_ptr() as *const libc::c_char,
                key.as_ptr() as *const libc::c_char,
                value.as_ptr() as *const libc::c_void,
                value.len(),
                0,
            )
        };
        if rc != 0 {
            return Err(format!(
                "{}: setting extended attribute {}: {}",
                at.display(),
                String::from_utf8_lossy(name),
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

/// Restore an entry's modification time, without following a symlink.
fn restore_mtime(at: &Path, seconds: u64) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let mut raw: Vec<u8> = at.as_os_str().as_bytes().to_vec();
    raw.push(0);
    let times = [
        libc::timespec {
            tv_sec: seconds as libc::time_t,
            tv_nsec: 0,
        },
        libc::timespec {
            tv_sec: seconds as libc::time_t,
            tv_nsec: 0,
        },
    ];
    // SAFETY: the path is nul-terminated and outlives the call, and `times`
    // is a two element array as the interface requires.
    let rc = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            raw.as_ptr() as *const libc::c_char,
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!(
            "{}: setting the modification time: {}",
            at.display(),
            std::io::Error::last_os_error()
        ))
    }
}

fn walk(
    r: &Reader<'_>,
    ino: u32,
    prefix: &Path,
    visit: &mut impl FnMut(&Path, u32) -> Result<(), String>,
) -> Result<(), String> {
    let children: Vec<(String, u32)> = r
        .entries(ino)
        .map(|(name, child)| (String::from_utf8_lossy(name).into_owned(), child))
        .collect();
    for (name, child) in children {
        let path = prefix.join(&name);
        visit(&path, child)?;
        walk(r, child, &path, visit)?;
    }
    Ok(())
}

fn check(located: Located) -> Result<(), String> {
    let image = located.open()?;
    let sb = *image.superblock();
    let budget = located
        .cache_budget
        .unwrap_or_else(|| image.default_budget());
    let reader = image
        .reader_with_budget(budget)
        .map_err(|e| e.to_string())?;

    let mut files = 0u64;
    let mut dirs = 0u64;
    let mut links = 0u64;
    let mut fifos = 0u64;
    let mut sockets = 0u64;
    let mut char_devices = 0u64;
    let mut block_devices = 0u64;
    let mut bytes = 0u64;
    let mut checked = 0u64;
    let t_verify = std::time::Instant::now();
    walk(&reader, 0, Path::new(""), &mut |_, ino| {
        let node = reader.inode(ino).ok_or("inode out of range")?;
        match node.file_type() {
            Some(FileType::Directory) => dirs += 1,
            Some(FileType::Symlink) => links += 1,
            Some(FileType::Fifo) => fifos += 1,
            Some(FileType::Socket) => sockets += 1,
            Some(FileType::CharDevice) => char_devices += 1,
            Some(FileType::BlockDevice) => block_devices += 1,
            Some(FileType::Regular) => {
                files += 1;
                bytes += node.size;
                let mut buf = vec![0u8; node.size.min(1 << 20) as usize];
                let mut at = 0u64;
                while at < node.size {
                    let n = reader.read(ino, at, &mut buf).map_err(|e| e.to_string())?;
                    if n == 0 {
                        return Err("read returned no bytes before the end of a file".into());
                    }
                    at += n as u64;
                    checked += n as u64;
                }
            }
            None => return Err(format!("inode {ino} has an unrecognised type")),
        }
        Ok(())
    })?;

    println!("format          {}.{}", sb.version_major, sb.version_minor);
    println!("image length    {}", sb.image_length);
    println!("stream length   {}", sb.stream_length);
    println!("block size      {}", sb.block_size);
    println!("blocks          {}", sb.block_count);
    println!("inodes          {}", sb.inode_count);
    println!("extents         {}", sb.extent_count);
    println!("codecs          {:#06x}", sb.codec_mask);

    use stipplefs::format::SectionKind;
    let sections = [
        ("inodes", SectionKind::Inodes),
        ("dirents", SectionKind::Dirents),
        ("names", SectionKind::Names),
        ("extents", SectionKind::Extents),
        ("blocks", SectionKind::Blocks),
        ("dictionary", SectionKind::Dictionaries),
        ("attributes", SectionKind::Attributes),
        ("xattrs", SectionKind::ExtendedAttributes),
    ];
    let mut metadata = 0u64;
    println!("sections");
    for (label, kind) in sections {
        let section = sb.section(kind);
        metadata += section.size;
        println!(
            "  {label:<12} {:>12} bytes at {:>12}",
            section.size, section.offset
        );
    }
    println!(
        "  {:<12} {:>12} bytes  ({:.1} per inode, {:.2}% of image)",
        "metadata",
        metadata,
        metadata as f64 / sb.inode_count.max(1) as f64,
        100.0 * metadata as f64 / sb.image_length as f64,
    );
    println!("directories     {dirs}");
    println!("files           {files} ({bytes} bytes)");
    println!("symlinks        {links}");
    println!("pipes           {fifos}");
    println!("sockets         {sockets}");
    println!("char devices    {char_devices}");
    println!("block devices   {block_devices}");
    let verify_elapsed = t_verify.elapsed();
    let mb_s = (checked as f64 / (1024.0 * 1024.0)) / verify_elapsed.as_secs_f64();
    println!(
        "verified        {checked} bytes read and checksummed in {:.3} s ({:.1} MB/s)",
        verify_elapsed.as_secs_f64(),
        mb_s
    );
    Ok(())
}

fn extract(located: Located, destination: PathBuf, subtree: Option<String>) -> Result<(), String> {
    let image = located.open()?;
    let budget = located
        .cache_budget
        .unwrap_or_else(|| image.default_budget());
    let reader = image
        .reader_with_budget(budget)
        .map_err(|e| e.to_string())?;
    let want = subtree.as_deref().map(Path::new);

    let mut wrote = 0u64;
    walk(&reader, 0, Path::new(""), &mut |path, ino| {
        if let Some(w) = want
            && !path.starts_with(w)
            && !w.starts_with(path)
        {
            return Ok(());
        }
        let node = reader.inode(ino).ok_or("inode out of range")?;
        let target = destination.join(path);
        match node.file_type() {
            Some(FileType::Directory) => {
                std::fs::create_dir_all(&target)
                    .map_err(|e| format!("{}: {e}", target.display()))?;
            }
            Some(FileType::Symlink) => {
                if want.is_some_and(|w| !path.starts_with(w)) {
                    return Ok(());
                }
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("{}: {e}", parent.display()))?;
                }
                let raw = reader.symlink_target(ino).ok_or("bad symlink target")?;
                let text = String::from_utf8_lossy(raw).into_owned();
                let _ = std::fs::remove_file(&target);
                std::os::unix::fs::symlink(&text, &target)
                    .map_err(|e| format!("{}: {e}", target.display()))?;
            }
            Some(
                kind @ (FileType::Fifo
                | FileType::Socket
                | FileType::CharDevice
                | FileType::BlockDevice),
            ) => {
                if want.is_some_and(|w| !path.starts_with(w)) {
                    return Ok(());
                }
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("{}: {e}", parent.display()))?;
                }
                let _ = std::fs::remove_file(&target);
                create_special(&target, kind, node.mode, node.device().unwrap_or(0))?;
            }
            Some(FileType::Regular) => {
                if want.is_some_and(|w| !path.starts_with(w)) {
                    return Ok(());
                }
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("{}: {e}", parent.display()))?;
                }
                let mut out = vec![0u8; node.size as usize];
                let mut at = 0usize;
                while (at as u64) < node.size {
                    let n = reader
                        .read(ino, at as u64, &mut out[at..])
                        .map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    at += n;
                }
                std::fs::write(&target, &out).map_err(|e| format!("{}: {e}", target.display()))?;
                wrote += node.size;
            }
            None => return Err(format!("inode {ino} has an unrecognised type")),
        }
        if want.is_none_or(|w| path.starts_with(w)) {
            let attrs = reader.xattrs(ino).map_err(|e| e.to_string())?;
            restore_xattrs(&target, &attrs)?;
            if let Some(t) = node.mtime {
                restore_mtime(&target, t)?;
            }
        }
        Ok(())
    })?;
    eprintln!("extracted {wrote} bytes into {}", destination.display());
    Ok(())
}

#[cfg(feature = "mount")]
fn mount(
    located: Located,
    mountpoint: PathBuf,
    allow_other: bool,
    threads: usize,
) -> Result<(), String> {
    use fuser::{Config, MountOption, SessionACL};

    let image = located.open()?;
    let budget = located
        .cache_budget
        .unwrap_or_else(|| image.default_budget());
    let fs = stipplefs::mount::Mount::new(image, budget).map_err(|e| e.to_string())?;

    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::RO,
        MountOption::FSName("stipplefs".into()),
        MountOption::Subtype("stipplefs".into()),
        MountOption::NoAtime,
    ];
    config.acl = if allow_other {
        SessionACL::All
    } else {
        SessionACL::Owner
    };
    config.n_threads = Some(threads.max(1));
    config.clone_fd = threads > 1;
    eprintln!("mounted on {}, interrupt to unmount", mountpoint.display());
    fuser::mount(fs, &mountpoint, &config).map_err(|e| format!("mounting: {e}"))
}

fn bench(located: Located, reads: usize, read_size: usize, seed: u64) -> Result<(), String> {
    let t_open = std::time::Instant::now();
    let image = located.open()?;
    let budget = located
        .cache_budget
        .unwrap_or_else(|| image.default_budget());
    let reader = image
        .reader_with_budget(budget)
        .map_err(|e| e.to_string())?;
    let open_elapsed = t_open.elapsed();

    let mut files: Vec<(u32, u64)> = Vec::new();
    walk(&reader, 0, Path::new(""), &mut |_, ino| {
        if let Some(node) = reader.inode(ino)
            && node.file_type() == Some(FileType::Regular)
            && node.size >= read_size as u64
        {
            files.push((ino, node.size));
        }
        Ok(())
    })?;
    if files.is_empty() {
        return Err(format!("no files of at least {read_size} B in the image"));
    }

    let mut state = seed | 1;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let picks: Vec<(u32, u64)> = (0..reads)
        .map(|_| {
            let (ino, size) = files[(next() % files.len() as u64) as usize];
            (ino, (next() % (size - read_size as u64).max(1)) & !4095)
        })
        .collect();

    let mut buf = vec![0u8; read_size];

    // Measure cold reads
    let mut cold_latencies = Vec::with_capacity(reads);
    let mut got = 0u64;
    let t_first = std::time::Instant::now();
    let mut first_read_elapsed = None;

    let start_cold = std::time::Instant::now();
    for (i, (ino, offset)) in picks.iter().enumerate() {
        let t0 = std::time::Instant::now();
        let n = reader
            .read(*ino, *offset, &mut buf)
            .map_err(|e| e.to_string())?;
        let lat = t0.elapsed();
        if i == 0 {
            first_read_elapsed = Some(t_first.elapsed());
        }
        got += n as u64;
        cold_latencies.push(lat);
    }
    let elapsed_cold = start_cold.elapsed();

    // Measure warm reads (run same picks again on warm cache)
    let mut warm_latencies = Vec::with_capacity(reads);
    let start_warm = std::time::Instant::now();
    for (ino, offset) in &picks {
        let t0 = std::time::Instant::now();
        let _ = reader
            .read(*ino, *offset, &mut buf)
            .map_err(|e| e.to_string())?;
        warm_latencies.push(t0.elapsed());
    }
    let elapsed_warm = start_warm.elapsed();

    cold_latencies.sort();
    warm_latencies.sort();

    let p = |l: &[std::time::Duration], pct: usize| -> f64 {
        if l.is_empty() {
            return 0.0;
        }
        let idx = (l.len() * pct / 100).min(l.len() - 1);
        l[idx].as_secs_f64() * 1e6 // in microseconds
    };

    println!("files eligible {}", files.len());
    println!("reads          {reads} (size {read_size} B)");
    println!("bytes          {got}");
    println!("open time      {:.3} ms", open_elapsed.as_secs_f64() * 1e3);
    println!(
        "first read     {:.3} ms",
        first_read_elapsed.unwrap_or_default().as_secs_f64() * 1e3
    );
    println!(
        "cold elapsed   {:.3} s ({:.1} reads/s)",
        elapsed_cold.as_secs_f64(),
        reads as f64 / elapsed_cold.as_secs_f64()
    );
    println!("cold p50       {:.1} us", p(&cold_latencies, 50));
    println!("cold p90       {:.1} us", p(&cold_latencies, 90));
    println!("cold p99       {:.1} us", p(&cold_latencies, 99));
    println!(
        "warm elapsed   {:.3} s ({:.1} reads/s)",
        elapsed_warm.as_secs_f64(),
        reads as f64 / elapsed_warm.as_secs_f64()
    );
    println!("warm p50       {:.1} us", p(&warm_latencies, 50));
    println!("warm p90       {:.1} us", p(&warm_latencies, 90));
    println!("warm p99       {:.1} us", p(&warm_latencies, 99));
    println!();
    print!("{}", reader.metrics().report());
    Ok(())
}

fn main() -> ExitCode {
    let args = Args::parse();
    let outcome = match args.command {
        Command::Check(located) => check(located),
        Command::Bench {
            image,
            reads,
            read_size,
            seed,
        } => bench(image, reads, read_size, seed),
        #[cfg(feature = "mount")]
        Command::Mount {
            image,
            mountpoint,
            allow_other,
            threads,
        } => mount(image, mountpoint, allow_other, threads),
        Command::Extract {
            image,
            destination,
            subtree,
        } => extract(image, destination, subtree),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("stipple: {message}");
            ExitCode::FAILURE
        }
    }
}
