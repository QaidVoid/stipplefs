//! Builds stipplefs images from a directory tree.

use clap::Parser;
use std::io::Write;
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
    /// Rough ceiling on memory used to compress blocks, in bytes. Cannot go
    /// below one block. Output does not depend on this.
    #[arg(long)]
    memory: Option<usize>,
    /// What to record for modification times. `preserve` keeps each entry's
    /// own time; `zero` records one fixed value, which is what makes two
    /// builds of the same content byte-identical across machines.
    #[arg(long, value_enum, default_value_t = MtimePolicy::Preserve)]
    mtime: MtimePolicy,
    /// How to order files before placing them into the stream.
    #[arg(long, value_enum, default_value_t = OrderArg::Walk)]
    order: OrderArg,
}

/// How `--order` is spelled on the command line.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum OrderArg {
    /// Pre-order tree walk order (format canonical default).
    Walk,
    /// Group files by extension and filename.
    Ext,
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
        memory_limit: args.memory,
        timestamps: match args.mtime {
            MtimePolicy::Preserve => Timestamps::Preserve,
            MtimePolicy::Zero => Timestamps::Clamp,
        },
    };

    let mut builder = Builder::new(options).map_err(|e| e.to_string())?;
    builder.set_order(match args.order {
        OrderArg::Walk => stipplefs::write::FileOrder::Walk,
        OrderArg::Ext => stipplefs::write::FileOrder::Extension,
    });
    builder
        .add_tree(&args.source)
        .map_err(|e| format!("reading {}: {e}", args.source.display()))?;
    let counts = builder.counts();

    // Write beside the target and rename, so a failure leaves no partial image.
    let temp = args.output.with_extension("stipple-partial");
    let written = (|| -> Result<u64, String> {
        let file =
            std::fs::File::create(&temp).map_err(|e| format!("writing {}: {e}", temp.display()))?;
        let mut out = std::io::BufWriter::new(file);
        builder.build_into(&mut out).map_err(|e| e.to_string())?;
        out.flush()
            .map_err(|e| format!("writing {}: {e}", temp.display()))?;
        let size = out
            .get_ref()
            .metadata()
            .map_err(|e| format!("writing {}: {e}", temp.display()))?
            .len();
        Ok(size)
    })()
    .inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })?;
    std::fs::rename(&temp, &args.output).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("renaming into {}: {e}", args.output.display())
    })?;

    eprintln!(
        "wrote {} ({} bytes) from {}",
        args.output.display(),
        written,
        args.source.display()
    );
    for (label, n) in counts.by_kind() {
        if n > 0 {
            eprintln!("  {label:<14} {n}");
        }
    }
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
