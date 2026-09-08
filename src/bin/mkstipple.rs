//! Builds stipplefs images from a directory tree.

use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;
use stipplefs::format::Codec;
use stipplefs::write::{Builder, Options, Timestamps};

#[derive(Parser)]
#[command(
    name = "mkstipple",
    about = "Build a stipplefs image from a directory tree"
)]
struct Args {
    /// Directory to build from.
    source: PathBuf,
    /// Image to write.
    output: PathBuf,
    /// Decoded size of a block, in bytes.
    #[arg(long, default_value_t = 65536)]
    block_size: u32,
    /// Target average deduplication chunk size, in bytes.
    #[arg(long, default_value_t = 65536)]
    chunk_size: u32,
    /// Compression level.
    #[arg(long, default_value_t = 19)]
    level: i32,
    /// Store everything uncompressed.
    #[arg(long)]
    no_compress: bool,
    /// Largest dictionary to train, in bytes. Zero disables training.
    #[arg(long, default_value_t = 112 * 1024)]
    dictionary_size: usize,
    /// Worker threads. Output does not depend on this.
    #[arg(long, default_value_t = 1)]
    threads: usize,
    /// What to record for modification times. `preserve` keeps each entry's
    /// own time; `zero` records one fixed value, which is what makes two
    /// builds of the same content byte-identical across machines.
    #[arg(long, value_enum, default_value_t = MtimePolicy::Preserve)]
    mtime: MtimePolicy,
}

/// How `--mtime` is spelled on the command line.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum MtimePolicy {
    /// Record each entry's own modification time.
    Preserve,
    /// Record one fixed value for every entry.
    Zero,
}

fn run(args: Args) -> Result<(), String> {
    if !args.source.is_dir() {
        return Err(format!("{} is not a directory", args.source.display()));
    }
    let codecs = if args.no_compress {
        Codec::Stored.bit()
    } else {
        Codec::Stored.bit() | Codec::Zstd.bit() | Codec::ZstdDict.bit()
    };
    let options = Options {
        block_size: args.block_size,
        chunk_average: args.chunk_size,
        codecs,
        level: args.level,
        dictionary_size: if args.no_compress {
            0
        } else {
            args.dictionary_size
        },
        threads: args.threads.max(1),
        timestamps: match args.mtime {
            MtimePolicy::Preserve => Timestamps::Preserve,
            MtimePolicy::Zero => Timestamps::Clamp,
        },
    };

    let mut builder = Builder::new(options).map_err(|e| e.to_string())?;
    builder
        .add_tree(&args.source)
        .map_err(|e| format!("reading {}: {e}", args.source.display()))?;
    let image = builder.build().map_err(|e| e.to_string())?;

    // Write beside the target and rename, so a failure leaves no partial image.
    let temp = args.output.with_extension("stipple-partial");
    std::fs::write(&temp, &image).map_err(|e| format!("writing {}: {e}", temp.display()))?;
    std::fs::rename(&temp, &args.output).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("renaming into {}: {e}", args.output.display())
    })?;

    eprintln!(
        "wrote {} ({} bytes) from {}",
        args.output.display(),
        image.len(),
        args.source.display()
    );
    Ok(())
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("mkstipple: {message}");
            ExitCode::FAILURE
        }
    }
}
