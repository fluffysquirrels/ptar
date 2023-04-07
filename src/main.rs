use clap::Parser;
use ignore::{DirEntry, WalkBuilder, WalkState};
use std::{
    fs::{self, File},
    io::BufWriter,
    path::PathBuf,
    result::Result as StdResult,
};

#[derive(clap::Parser)]
struct Args {
    #[arg(long)]
    in_path: PathBuf,
    #[arg(long)]
    out_dir: PathBuf,
    #[arg(long)]
    threads: usize,
    #[arg(long)]
    log_json: bool,
}

#[derive(Eq, PartialEq)]
enum LogMode {
    Pretty,
    Json,
}

struct PVB {
    in_path: PathBuf,
    in_prefix: PathBuf,
    next_archive_number: u64,
    out_dir: PathBuf,
}

struct ErrorPV;

struct PV {
    in_prefix: PathBuf,
    out_path: PathBuf,
    /// Always Some(_) except in the drop implementation.
    tarb: Option<tar::Builder<zstd::stream::write::Encoder<'static, BufWriter<File>>>>,
}

type Error = anyhow::Error;
type Result<T> = std::result::Result<T, Error>;

const ZSTD_DEFAULT_COMPRESSION_LEVEL: i32 = 0;

fn main() -> Result<()> {
    let args = Args::parse();

    init_logging(args.log_json)?;

    let in_meta = args.in_path.metadata()?;
    let (in_prefix, in_path) = if in_meta.is_dir() {
        (args.in_path.clone(), args.in_path.clone())
    } else {
        match args.in_path.parent() {
            Some(parent) => (parent.to_path_buf(), args.in_path.clone()),
            None => (PathBuf::from("./"), PathBuf::from("./").join(&*args.in_path)),
        }
    };

    fs::create_dir_all(&*args.out_dir)?;

    let walker =
        WalkBuilder::new(&*in_path)
                    .threads(args.threads)
                    .standard_filters(false)
                    .build_parallel();

    walker.visit(&mut PVB {
        in_path: in_path,
        in_prefix: in_prefix,
        next_archive_number: 0,
        out_dir: args.out_dir,
    });

    Ok(())
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
        let archive_num = self.next_archive_number;
        self.next_archive_number += 1;
        let out_file_path = self.out_dir.join(format!("{archive_num:08}.tar.zstd"));

        // Closure to capture errors returned with `?`.
        let res = (|| -> Result<PV> {
            let file = fs::OpenOptions::new()
                                       .write(true)
                                       .create_new(true)
                                       .open(&*out_file_path)?;
            let bufw = BufWriter::with_capacity(128 * 1024, file);
            let zstdw = zstd::stream::write::Encoder::new(bufw, ZSTD_DEFAULT_COMPRESSION_LEVEL)?;
            let tarb = tar::Builder::new(zstdw);
            Ok(PV {
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
                return WalkState::Quit;
            }
        };
        if let Err(err) = self.tarb.as_mut().expect("PV.tarb always Some except in drop")
                                   .append_path_with_name(path, rel_path) {
            tracing::error!(path = %path.display(), %err, "Error appending file");
            return WalkState::Quit;
        }

        WalkState::Continue
    }
}

impl Drop for PV {
    fn drop(&mut self) {
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

        if let Err(err) = res {
            tracing::error!(%err, out_path = %self.out_path.display(),
                            "Error while closing archive in PV::drop()");
        }
    }
}
