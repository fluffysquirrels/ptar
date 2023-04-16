// Declare this first so other modules can use the macro.
#[macro_use]
mod lazy_regex;

mod compress;
mod decompress;

use clap::Parser;
use std::time::Instant;
use valuable::Valuable;

#[derive(clap::Parser, Valuable)]
pub struct Args {
    #[arg(long)]
    threads: usize,
    #[arg(long)]
    log_json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand, Clone, Debug, Valuable)]
pub enum Command {
    Compress(compress::Args),
    Decompress(decompress::Args),
}

#[derive(Eq, PartialEq)]
enum LogMode {
    Pretty,
    Json,
}

type Error = anyhow::Error;
type Result<T> = std::result::Result<T, Error>;

fn main() -> Result<()> {
    let start = Instant::now();

    let args = Args::parse();

    init_logging(args.log_json)?;

    tracing::info!(args = args.as_value(), "Starting");

    let res = match &args.command {
        Command::Compress(cmd_args) => compress::main(cmd_args.clone(), args),
        Command::Decompress(cmd_args) => decompress::main(cmd_args.clone(), args),
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
