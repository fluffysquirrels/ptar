use anyhow::ensure;
use clap::Parser;
use ignore::{DirEntry, WalkBuilder, WalkState};
use std::{
    fs::{self, File},
    io::BufWriter,
    path::PathBuf,
    result::Result as StdResult,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};
use valuable::Valuable;

#[derive(clap::Parser, Valuable)]
struct Args {
    #[arg(long)]
    threads: usize,
    #[arg(long)]
    log_json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand, Clone, Debug, Valuable)]
enum Command {
    Compress(CompressArgs),
    Decompress(DecompressArgs),
}

#[derive(clap::Args, Clone, Debug, Valuable)]
struct CompressArgs {
    #[arg(long)]
    in_path: PathBuf,
    #[arg(long)]
    out_dir: PathBuf,
}

#[derive(clap::Args, Clone, Debug, Valuable)]
struct DecompressArgs {
    #[arg(long)]
    in_dir: PathBuf,
    #[arg(long)]
    out_dir: PathBuf,
}

#[derive(Eq, PartialEq)]
enum LogMode {
    Pretty,
    Json,
}

struct PVB {
    error_count: Arc<AtomicUsize>,
    in_path: PathBuf,
    in_prefix: PathBuf,
    next_archive_num: u64,
    out_dir: PathBuf,
}

struct ErrorPV;

struct PV {
    archive_num: u64,
    error_count: Arc<AtomicUsize>,
    in_prefix: PathBuf,
    out_path: PathBuf,
    /// Always Some(_) except in the drop implementation.
    tarb: Option<tar::Builder<zstd::stream::write::Encoder<'static, BufWriter<File>>>>,
}

type Error = anyhow::Error;
type Result<T> = std::result::Result<T, Error>;

const ZSTD_DEFAULT_COMPRESSION_LEVEL: i32 = 0;

fn main() -> Result<()> {
    let start = Instant::now();

    let args = Args::parse();

    init_logging(args.log_json)?;

    tracing::info!(args = args.as_value(), "Starting");

    let res = match &args.command {
        Command::Compress(cmd_args) => compress(cmd_args.clone(), args),
        Command::Decompress(cmd_args) => decompress(cmd_args.clone(), args),
    };

    if let Err(err) = res {
        // tracing::error! to show it nicely formatted, potentially in JSON.
        tracing::error!(err = %err, "Error");
        // Return the error too to show a Rust backtrace on the CLI.
        return Err(err);
    }

    tracing::info!(duration_ms = start.elapsed().as_millis(), "Done");

    Ok(())
}

fn compress(cmd_args: CompressArgs, args: Args) -> Result<()> {
    let in_meta = cmd_args.in_path.metadata()?;
    let (in_prefix, in_path) = if in_meta.is_dir() {
        (cmd_args.in_path.clone(), cmd_args.in_path.clone())
    } else {
        match cmd_args.in_path.parent() {
            Some(parent) => (parent.to_path_buf(), cmd_args.in_path.clone()),
            None => (PathBuf::from("./"), PathBuf::from("./").join(&*cmd_args.in_path)),
        }
    };

    fs::create_dir_all(&*cmd_args.out_dir)?;

    let walker =
        WalkBuilder::new(&*in_path)
                    .threads(args.threads)
                    .standard_filters(false)
                    .build_parallel();

    let error_count = Arc::new(AtomicUsize::new(0));

    walker.visit(&mut PVB {
        error_count: error_count.clone(),
        in_path: in_path,
        in_prefix: in_prefix,
        next_archive_num: 0,
        out_dir: cmd_args.out_dir,
    });

    let final_error_count = error_count.load(Ordering::SeqCst);
    ensure!(final_error_count == 0, "Errors in compress() count={final_error_count}");

    Ok(())
}

fn decompress(_cmd_args: DecompressArgs, _args: Args) -> Result<()> {
    todo!();
}

fn init_logging(log_json: bool) -> Result<()> {
    use tracing_bunyan_formatter::{
        BunyanFormattingLayer,
        JsonStorageLayer,
    };
    use tracing_subscriber::{
        EnvFilter,
        filter::LevelFilter,
        fmt,
        prelude::*,
    };

    let log_mode = if log_json { LogMode::Json } else { LogMode::Pretty };

    tracing_subscriber::Registry::default()
        .with(if log_mode == LogMode::Pretty {
                  Some(fmt::Layer::new()
                           .event_format(fmt::format()
                                             .pretty()
                                             .with_timer(fmt::time::UtcTime::<_>::
                                                             rfc_3339())
                                             .with_target(true)
                                             .with_source_location(true)
                                             .with_thread_ids(true))
                           .with_writer(std::io::stderr)
                           .with_span_events(fmt::format::FmtSpan::NEW
                                             | fmt::format::FmtSpan::CLOSE))
              } else {
                  None
              })
        .with(if log_mode == LogMode::Json {
                  Some(JsonStorageLayer
                           .and_then(BunyanFormattingLayer::new(
                               env!("CARGO_CRATE_NAME").to_string(),
                               std::io::stderr)))
              } else {
                  None
              })
        // Global filter
        .with(EnvFilter::builder()
                  .with_default_directive(LevelFilter::INFO.into())
                  .parse(std::env::var("RUST_LOG")
                             .unwrap_or(format!("warn,{crate_}=info",
                                                crate_ = env!("CARGO_CRATE_NAME"))))?)
        .try_init()?;

    Ok(())
}

impl ignore::ParallelVisitorBuilder<'static> for PVB {
    /// Build a visitor for an ignore thread.
    fn build(&mut self) -> Box<dyn ignore::ParallelVisitor + 'static> {
        let archive_num = self.next_archive_num;
        self.next_archive_num += 1;
        let out_file_path = self.out_dir.join(format!("{archive_num:08}.tar.zstd"));

        // Closure to capture errors returned with `?`.
        let res = (|| -> Result<PV> {
            let file = fs::OpenOptions::new()
                                       .write(true)
                                       .create_new(true)
                                       .open(&*out_file_path)?;
            let bufw = BufWriter::with_capacity(128 * 1024, file);
            let mut zstdw = zstd::stream::write::Encoder::new(bufw,
                                                              ZSTD_DEFAULT_COMPRESSION_LEVEL)?;
            // Compression will be done in a separate thread, to detach I/O and compression.
            zstdw.multithread(1)?;
            let tarb = tar::Builder::new(zstdw);
            Ok(PV {
                archive_num,
                error_count: self.error_count.clone(),
                in_prefix: self.in_prefix.clone(),
                out_path: out_file_path.to_path_buf(),
                tarb: Some(tarb),
            })
        })();

        match res {
            Err(err) => {
                tracing::error!(in_path = %self.in_path.display(),
                                out_file_path = %out_file_path.display(),
                                archive_num,
                                %err,
                                "Error creating ParallelVisitor");
                let _ = self.error_count.fetch_add(1, Ordering::SeqCst);
                Box::new(ErrorPV)
            },
            Ok(pv) => Box::new(pv),
        }
    }
}

impl ignore::ParallelVisitor for ErrorPV {
    fn visit(&mut self, _entry: StdResult<DirEntry, ignore::Error>) -> WalkState {
        WalkState::Quit
    }
}

impl ignore::ParallelVisitor for PV {
    fn visit(&mut self, entry: StdResult<DirEntry, ignore::Error>) -> WalkState {
        let entry = match entry {
            Err(err) => {
                tracing::warn!(%err, "Error given to PV.visit");
                let _ = self.error_count.fetch_add(1, Ordering::SeqCst);
                return WalkState::Continue;
            },
            Ok(v) => v,
        };
        let Some(file_type) = entry.file_type() else {
            return WalkState::Continue;
        };
        if !file_type.is_file() {
            return WalkState::Continue;
        }
        // It's a file.
        let path = entry.path();
        let rel_path = match path.strip_prefix(&*self.in_prefix) {
            Ok(p) => p,
            Err(err) => {
                tracing::error!(path = %path.display(),
                                prefix = %self.in_prefix.display(),
                                %err,
                                "Error stripping path prefix");
                let _ = self.error_count.fetch_add(1, Ordering::SeqCst);
                return WalkState::Quit;
            }
        };
        if let Err(err) = self.tarb.as_mut().expect("PV.tarb always Some except in drop")
                                   .append_path_with_name(path, rel_path) {
            tracing::error!(path = %path.display(), %err, "Error appending file");
            let _ = self.error_count.fetch_add(1, Ordering::SeqCst);
            return WalkState::Quit;
        }

        WalkState::Continue
    }
}

impl Drop for PV {
    fn drop(&mut self) {
        tracing::debug!(archive_num = self.archive_num,
                        "PV::drop start");

        // Closure to catch errors with `?`.
        let res = (|| -> Result<()> {
            let tarb = self.tarb.take();
            // tarb.into_inner() finishes writing the tar archive.
            let zstdw: zstd::stream::write::Encoder<_> =
                tarb.expect("PV.tarb always Some except in drop")
                    .into_inner()?;
            let bufw = zstdw.finish()?;
            let file = bufw.into_inner()
                           .map_err(|err| err.into_error())?;
            file.sync_all()?;

            Ok(())
        })();

        tracing::debug!(archive_num = self.archive_num,
                        "PV::drop complete");

        if let Err(err) = res {
            tracing::error!(%err, out_path = %self.out_path.display(),
                            "Error while closing archive in PV::drop()");
            let _ = self.error_count.fetch_add(1, Ordering::SeqCst);
        }
    }
}
