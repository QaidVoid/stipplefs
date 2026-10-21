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
        /// Background threads decoding blocks and storing their file bytes
        /// in the kernel's page cache, defaulting to one per core up to
        /// twelve. Zero serves every read on demand.
        #[arg(long)]
        warm_workers: Option<usize>,
        /// Blocks past each read to warm ahead of it.
        #[arg(long, default_value_t = 2)]
        warm_ahead: u32,
        /// Blocks at the front of the stream to warm as soon as the mount
        /// exists, defaulting to those holding the image's hot region.
        #[arg(long)]
        warm_start: Option<u32>,
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

/// Restore an entry's permission bits, which creating it filtered through
/// the umask.
fn restore_mode(at: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(at, std::fs::Permissions::from_mode(mode & 0o7777))
        .map_err(|e| format!("{}: setting permissions: {e}", at.display()))
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
    use std::os::unix::ffi::OsStrExt;
    let children: Vec<(&[u8], u32)> = r.entries(ino).collect();
    for (name, child) in children {
        let path = prefix.join(std::ffi::OsStr::from_bytes(name));
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
    println!("hot region      {} bytes", sb.hot_length);
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
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;

    let image = located.open()?;
    let budget = located
        .cache_budget
        .unwrap_or_else(|| image.default_budget());
    let reader = image
        .reader_with_budget(budget)
        .map_err(|e| e.to_string())?;
    let want = subtree.as_deref().map(Path::new);

    let mut wrote = 0u64;
    // Creating an entry changes its directory's time, and a read-only
    // directory refuses new entries, so directories get their mode and time
    // once everything inside them exists.
    let mut dirs: Vec<(PathBuf, u32, Option<u64>)> = Vec::new();
    // Where the first name of each hard-linked inode was extracted.
    let mut linked: std::collections::HashMap<u32, PathBuf> = std::collections::HashMap::new();
    let mut buf = vec![0u8; 1 << 20];
    walk(&reader, 0, Path::new(""), &mut |path, ino| {
        if let Some(w) = want
            && !path.starts_with(w)
            && !w.starts_with(path)
        {
            return Ok(());
        }
        let node = reader.inode(ino).ok_or("inode out of range")?;
        let target = destination.join(path);
        let wanted = want.is_none_or(|w| path.starts_with(w));
        let kind = node
            .file_type()
            .ok_or_else(|| format!("inode {ino} has an unrecognised type"))?;
        if kind == FileType::Directory {
            std::fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
            if wanted {
                let attrs = reader.xattrs(ino).map_err(|e| e.to_string())?;
                restore_xattrs(&target, &attrs)?;
                dirs.push((target, node.mode, node.mtime));
            }
            return Ok(());
        }
        if !wanted {
            return Ok(());
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let _ = std::fs::remove_file(&target);
        if let Some(first) = linked.get(&ino) {
            return std::fs::hard_link(first, &target)
                .map_err(|e| format!("{}: {e}", target.display()));
        }
        match kind {
            FileType::Symlink => {
                let raw = reader.symlink_target(ino).ok_or("bad symlink target")?;
                std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(raw), &target)
                    .map_err(|e| format!("{}: {e}", target.display()))?;
            }
            FileType::Regular => {
                let fail = |e: std::io::Error| format!("{}: {e}", target.display());
                let mut out = std::fs::File::create(&target).map_err(fail)?;
                let mut at = 0u64;
                while at < node.size {
                    let n = reader.read(ino, at, &mut buf).map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    out.write_all(&buf[..n]).map_err(fail)?;
                    at += n as u64;
                }
                wrote += at;
            }
            FileType::Directory => unreachable!("handled above"),
            special => create_special(&target, special, node.mode, node.device().unwrap_or(0))?,
        }
        let attrs = reader.xattrs(ino).map_err(|e| e.to_string())?;
        restore_xattrs(&target, &attrs)?;
        if kind != FileType::Symlink {
            restore_mode(&target, node.mode)?;
        }
        if let Some(t) = node.mtime {
            restore_mtime(&target, t)?;
        }
        if node.nlink > 1 {
            linked.insert(ino, target);
        }
        Ok(())
    })?;
    for (dir, mode, mtime) in dirs.iter().rev() {
        restore_mode(dir, *mode)?;
        if let Some(t) = mtime {
            restore_mtime(dir, *t)?;
        }
    }
    eprintln!("extracted {wrote} bytes into {}", destination.display());
    Ok(())
}

/// Most warming threads used unless asked for more. Past this, a starting
/// application was no faster and each extra thread held another block.
#[cfg(feature = "mount")]
const MAX_WARM_WORKERS: usize = 12;

/// Block the signals that ask the process to stop, in this thread and every
/// thread started from it afterwards, so only a `sigwait` on the returned set
/// receives them.
#[cfg(feature = "mount")]
fn block_stop_signals() -> libc::sigset_t {
    // SAFETY: the set is initialised by sigemptyset before any other use, and
    // pthread_sigmask only changes this thread's signal mask.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaddset(&mut set, signal);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        set
    }
}

#[cfg(feature = "mount")]
fn mount(
    located: Located,
    mountpoint: PathBuf,
    allow_other: bool,
    threads: usize,
    warm_workers: Option<usize>,
    warm_ahead: u32,
    warm_start: Option<u32>,
) -> Result<(), String> {
    // glibc raises its mmap threshold each time a large allocation is freed,
    // after which decoded blocks come from per-thread heaps that keep their
    // memory once the block is dropped. A fixed threshold keeps every block
    // in its own mapping, returned to the system as soon as it is freed.
    #[cfg(target_env = "gnu")]
    // SAFETY: mallopt only adjusts allocator tuning and is called before any
    // worker thread exists.
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, 1 << 20);
    }
    let stop = block_stop_signals();
    use fuser::{Config, MountOption, SessionACL};

    let image = located.open()?;
    let budget = located
        .cache_budget
        .unwrap_or_else(|| image.default_budget());
    let sb = *image.superblock();
    let warm_workers = warm_workers.unwrap_or_else(|| {
        std::thread::available_parallelism().map_or(1, |n| n.get().min(MAX_WARM_WORKERS))
    });
    let warm_start = warm_start.unwrap_or_else(|| {
        u32::try_from(sb.hot_length.div_ceil(sb.block_size as u64)).unwrap_or(u32::MAX)
    });
    let fs = stipplefs::mount::Mount::new(image, budget).map_err(|e| e.to_string())?;
    let (fs, notifier) = if warm_workers > 0 {
        let mountpoint = std::fs::canonicalize(&mountpoint)
            .map_err(|e| format!("{}: {e}", mountpoint.display()))?;
        let (fs, cell) = fs.with_warming(&mountpoint, warm_workers, warm_ahead, warm_start);
        (fs, Some(cell))
    } else {
        (fs, None)
    };

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
    let reader = fs.reader();
    eprintln!("mounted on {}, interrupt to unmount", mountpoint.display());
    let mut session =
        fuser::Session::new(fs, &mountpoint, &config).map_err(|e| format!("mounting: {e}"))?;
    // Dying on a signal would leave the mountpoint disconnected rather than
    // unmounted, so a signal unmounts instead and the session ends normally.
    let mut unmounter = session.unmount_callable();
    std::thread::spawn(move || {
        let mut signal = 0;
        // SAFETY: `stop` is an initialised signal set and `signal` is a valid
        // place for the received signal number.
        unsafe { libc::sigwait(&stop, &mut signal) };
        if let Err(e) = unmounter.unmount() {
            eprintln!("unmounting: {e}");
        }
    });
    if let Some(cell) = notifier {
        let _ = cell.set(session.notifier());
    }
    session.run().map_err(|e| format!("mounting: {e}"))?;
    if cfg!(feature = "metrics") {
        eprint!("{}", reader.metrics().report());
    }
    Ok(())
}

/// Perform `picks` in order, returning the total time, each read's latency
/// and the bytes read.
fn time_reads(
    reader: &Reader<'_>,
    picks: &[(u32, u64)],
    buf: &mut [u8],
) -> Result<(std::time::Duration, Vec<std::time::Duration>, u64), String> {
    let mut latencies = Vec::with_capacity(picks.len());
    let mut got = 0u64;
    let start = std::time::Instant::now();
    for &(ino, offset) in picks {
        let t = std::time::Instant::now();
        got += reader.read(ino, offset, buf).map_err(|e| e.to_string())? as u64;
        latencies.push(t.elapsed());
    }
    Ok((start.elapsed(), latencies, got))
}

/// The latency below which `pct` percent of sorted `latencies` fall, in
/// microseconds.
fn percentile(latencies: &[std::time::Duration], pct: usize) -> f64 {
    let Some(last) = latencies.len().checked_sub(1) else {
        return 0.0;
    };
    latencies[(latencies.len() * pct / 100).min(last)].as_secs_f64() * 1e6
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
    // The same reads twice: first against an empty cache, then a full one.
    let (cold_elapsed, mut cold, got) = time_reads(&reader, &picks, &mut buf)?;
    let first_read = cold.first().copied().unwrap_or_default();
    let (warm_elapsed, mut warm, _) = time_reads(&reader, &picks, &mut buf)?;
    cold.sort();
    warm.sort();

    println!("files eligible {}", files.len());
    println!("reads          {reads} (size {read_size} B)");
    println!("bytes          {got}");
    println!("open time      {:.3} ms", open_elapsed.as_secs_f64() * 1e3);
    println!("first read     {:.3} ms", first_read.as_secs_f64() * 1e3);
    for (label, elapsed, latencies) in
        [("cold", cold_elapsed, &cold), ("warm", warm_elapsed, &warm)]
    {
        println!(
            "{label} elapsed   {:.3} s ({:.1} reads/s)",
            elapsed.as_secs_f64(),
            reads as f64 / elapsed.as_secs_f64()
        );
        for pct in [50, 90, 99] {
            println!("{label} p{pct}       {:.1} us", percentile(latencies, pct));
        }
    }
    if cfg!(feature = "metrics") {
        println!();
        print!("{}", reader.metrics().report());
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = Args::parse();
    #[cfg(feature = "mount")]
    let mounting = matches!(args.command, Command::Mount { .. });
    #[cfg(not(feature = "mount"))]
    let mounting = false;
    // Rust ignores SIGPIPE, so output cut short by a closed pipe, as in
    // `stipple check image | head`, would panic instead of ending quietly. A
    // mount keeps ignoring it, so a closed log pipe cannot take it down.
    if !mounting {
        // SAFETY: restoring the default disposition of one signal, before any
        // other thread exists.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
    }
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
            warm_workers,
            warm_ahead,
            warm_start,
        } => mount(
            image,
            mountpoint,
            allow_other,
            threads,
            warm_workers,
            warm_ahead,
            warm_start,
        ),
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
