//! Builds stipplefs images from a directory tree.

use clap::Parser;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use stipplefs::format::Codec;
use stipplefs::write::{Builder, Options, Progress, Timestamps};

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
    /// Decoded size of a block, in bytes. Larger blocks compress better and
    /// cost more to decode on first access.
    #[arg(long, default_value_t = 32 << 20)]
    block_size: u32,
    /// Target average deduplication chunk size, in bytes. Small chunks find
    /// duplicates that larger ones straddle.
    #[arg(long, default_value_t = 4096)]
    chunk_size: u32,
    /// Compression level, for blocks and for compressed metadata.
    #[arg(long, default_value_t = 19)]
    level: i32,
    /// Store everything uncompressed, metadata included.
    #[arg(long)]
    no_compress: bool,
    /// Skip the x86 BCJ pre-filter, which drops both BCJ codecs.
    #[arg(long)]
    no_bcj: bool,
    /// Store metadata sections uncompressed.
    #[arg(long)]
    no_compress_metadata: bool,
    /// Largest dictionary to train, in bytes. Zero disables training. Defaults
    /// to 112 KiB for blocks up to 1 MiB and to none above that, where a
    /// dictionary no longer makes blocks smaller and only costs time.
    #[arg(long)]
    dictionary_size: Option<usize>,
    /// Worker threads, defaulting to one per available core. Output does not
    /// depend on this.
    #[arg(long)]
    threads: Option<usize>,
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
    #[arg(long, value_enum, default_value_t = OrderArg::Ext)]
    order: OrderArg,
    /// File naming, one path per line relative to the source, the files to
    /// place first in the stream, in that order. Typically the files an
    /// application opens while starting, in the order it opens them.
    #[arg(long)]
    hot_list: Option<PathBuf>,
}

/// Largest block size that trains a dictionary unless told otherwise.
const DICTIONARY_BLOCK_LIMIT: u32 = 1 << 20;

/// How `--order` is spelled on the command line.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum OrderArg {
    /// Pre-order tree walk order, the format's canonical encoding.
    Walk,
    /// Group files by extension and filename, which compresses better.
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
    if args.no_compress && args.no_bcj {
        return Err("--no-compress already excludes every other codec".into());
    }
    let codecs = if args.no_compress {
        Codec::Stored.bit()
    } else if args.no_bcj {
        Codec::Stored.bit() | Codec::Zstd.bit() | Codec::ZstdDict.bit()
    } else {
        Codec::Stored.bit()
            | Codec::Zstd.bit()
            | Codec::ZstdDict.bit()
            | Codec::ZstdBcj.bit()
            | Codec::ZstdDictBcj.bit()
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
                .unwrap_or(if args.block_size <= DICTIONARY_BLOCK_LIMIT {
                    112 * 1024
                } else {
                    0
                })
        },
        threads: args
            .threads
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
            .max(1),
        memory_limit: args.memory,
        timestamps: match args.mtime {
            MtimePolicy::Preserve => Timestamps::Preserve,
            MtimePolicy::Zero => Timestamps::Clamp,
        },
        compress_metadata: !args.no_compress && !args.no_compress_metadata,
    };

    eprintln!(
        "building {} from {}: block size {}, level {}, {} threads",
        args.output.display(),
        args.source.display(),
        options.block_size,
        options.level,
        options.threads,
    );
    let mut builder = Builder::new(options).map_err(|e| e.to_string())?;
    builder.set_progress(progress_printer());
    builder.set_order(match args.order {
        OrderArg::Walk => stipplefs::write::FileOrder::Walk,
        OrderArg::Ext => stipplefs::write::FileOrder::Extension,
    });
    if let Some(list) = &args.hot_list {
        let text = std::fs::read(list).map_err(|e| format!("reading {}: {e}", list.display()))?;
        builder.set_hot_list(text.split(|&b| b == b'\n').filter(|l| !l.is_empty()));
    }
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

/// Print build progress to standard error.
///
/// A terminal gets one line rewritten in place a few times a second. Anything
/// else, such as a CI log, gets a line when a stage starts or ends and every
/// few seconds between, so the log stays readable.
fn progress_printer() -> impl Fn(Progress) + Send + Sync + 'static {
    use std::io::IsTerminal;
    use std::time::{Duration, Instant};

    struct State {
        started: Instant,
        compressing_since: Option<Instant>,
        printed_at: Option<Instant>,
        stage: Option<std::mem::Discriminant<Progress>>,
        open_line: bool,
    }

    let tty = std::io::stderr().is_terminal();
    let interval = Duration::from_millis(if tty { 100 } else { 5000 });
    let state = std::sync::Mutex::new(State {
        started: Instant::now(),
        compressing_since: None,
        printed_at: None,
        stage: None,
        open_line: false,
    });
    let mib = |b: u64| b as f64 / (1024.0 * 1024.0);

    move |p| {
        let Ok(mut st) = state.lock() else { return };
        let now = Instant::now();
        let stage = std::mem::discriminant(&p);
        let changed = st.stage != Some(stage);
        let finished = match p {
            Progress::Placing {
                files, total_files, ..
            } => files == total_files,
            Progress::Compressing {
                blocks,
                total_blocks,
                ..
            } => blocks == total_blocks,
            Progress::Training | Progress::Writing => true,
        };
        let due = st.printed_at.is_none_or(|t| now - t >= interval);
        if !changed && !finished && !due {
            return;
        }
        if matches!(p, Progress::Compressing { .. }) && st.compressing_since.is_none() {
            st.compressing_since = Some(now);
        }
        let line = match p {
            Progress::Placing {
                files,
                total_files,
                bytes,
            } => format!(
                "placing      {files}/{total_files} files, {:.1} MiB read",
                mib(bytes)
            ),
            Progress::Training => "training     dictionary".to_string(),
            Progress::Compressing {
                blocks,
                total_blocks,
                input_bytes,
                output_bytes,
            } => {
                let secs = st
                    .compressing_since
                    .map_or(0.0, |t| (now - t).as_secs_f64())
                    .max(1e-3);
                format!(
                    "compressing  {blocks}/{total_blocks} blocks, {:.1} -> {:.1} MiB ({:.1}%), {:.1} MiB/s",
                    mib(input_bytes),
                    mib(output_bytes),
                    100.0 * output_bytes as f64 / input_bytes.max(1) as f64,
                    mib(input_bytes) / secs,
                )
            }
            Progress::Writing => "writing      image".to_string(),
        };
        let elapsed = (now - st.started).as_secs_f64();
        if tty {
            if changed && st.open_line {
                eprintln!();
            }
            eprint!("\r\x1b[K[{elapsed:7.1}s] {line}");
            st.open_line = !finished;
            if finished {
                eprintln!();
            }
        } else {
            eprintln!("[{elapsed:7.1}s] {line}");
        }
        st.printed_at = Some(now);
        st.stage = Some(stage);
    }
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
