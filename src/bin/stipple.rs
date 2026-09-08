//! Reads stipplefs images: inspection, extraction and mounting.

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use stipplefs::format::FileType;
use stipplefs::read::{DEFAULT_CACHE_BUDGET, Image, Reader};

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
    /// Bytes of decoded blocks the reader may retain.
    #[arg(long, default_value_t = DEFAULT_CACHE_BUDGET)]
    cache_budget: usize,
}

impl Located {
    fn open(&self) -> Result<Image<'static>, String> {
        let size = std::fs::metadata(&self.path)
            .map_err(|e| format!("{}: {e}", self.path.display()))?
            .len();
        let length = self.length.unwrap_or(size.saturating_sub(self.offset));
        Image::open_at(&self.path, self.offset, length)
            .map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

fn walk(
    r: &Reader<'_>,
    ino: u32,
    prefix: &Path,
    visit: &mut impl FnMut(&Path, u32) -> Result<(), String>,
) -> Result<(), String> {
    let node = r.inode(ino).ok_or("inode out of range")?;
    if node.file_type() != Some(FileType::Directory) {
        return Ok(());
    }
    for i in 0..node.size as u32 {
        let entry = r
            .dirent(node.payload as u32 + i)
            .ok_or("dirent out of range")?;
        let name = r.name(entry).ok_or("name out of range")?;
        let name = String::from_utf8_lossy(name).into_owned();
        let path = prefix.join(&name);
        visit(&path, entry.inode)?;
        walk(r, entry.inode, &path, visit)?;
    }
    Ok(())
}

fn check(located: Located) -> Result<(), String> {
    let image = located.open()?;
    let sb = *image.superblock();
    let reader = image
        .reader_with_budget(located.cache_budget)
        .map_err(|e| e.to_string())?;

    let mut files = 0u64;
    let mut dirs = 0u64;
    let mut links = 0u64;
    let mut bytes = 0u64;
    let mut checked = 0u64;
    walk(&reader, 0, Path::new(""), &mut |_, ino| {
        let node = reader.inode(ino).ok_or("inode out of range")?;
        match node.file_type() {
            Some(FileType::Directory) => dirs += 1,
            Some(FileType::Symlink) => links += 1,
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
    println!("directories     {dirs}");
    println!("files           {files} ({bytes} bytes)");
    println!("symlinks        {links}");
    println!("verified        {checked} bytes read and checksummed");
    Ok(())
}

fn extract(located: Located, destination: PathBuf, subtree: Option<String>) -> Result<(), String> {
    let image = located.open()?;
    let reader = image
        .reader_with_budget(located.cache_budget)
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
        Ok(())
    })?;
    eprintln!("extracted {wrote} bytes into {}", destination.display());
    Ok(())
}

fn main() -> ExitCode {
    let args = Args::parse();
    let outcome = match args.command {
        Command::Check(located) => check(located),
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
