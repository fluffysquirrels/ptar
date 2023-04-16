use crate::Result;
use progress_streams::ProgressReader;
use rayon::prelude::*;
// use spsc_bip_buffer as bip;
use std::{
    fs::{self, File},
    io::BufReader,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use valuable::Valuable;

#[derive(clap::Args, Clone, Debug, Valuable)]
pub struct Args {
    #[arg(long)]
    in_dir: PathBuf,
    #[arg(long)]
    out_dir: PathBuf,
}

pub fn main(cmd_args: Args, args: crate::Args) -> Result<()> {
    let mut archive_paths = Vec::<PathBuf>::with_capacity(args.threads + 1);

    for entry in fs::read_dir(&*cmd_args.in_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        if !lazy_regex!(".tar.zstd$").is_match(&*entry.file_name().to_string_lossy()) {
            continue;
        }
        archive_paths.push(entry.path());
    }

    archive_paths.sort();

    tracing::debug!(len = archive_paths.len(), ?archive_paths, "Enumerated archive paths");

    rayon::ThreadPoolBuilder::new()
        .num_threads(args.threads)
        .build()?
        .install(|| -> Result<()> {
            archive_paths
                .into_par_iter()
                .with_max_len(1) // 1 item per thread
                .try_for_each(|archive_path: PathBuf| -> Result<()> {
                    let _thread_span = tracing::debug_span!(
                        "decompress() thread",
                        archive_file_name = &*archive_path.file_name()
                            .expect("archive_path.file_name().is_some()")
                            .to_string_lossy()
                    ).entered();

                    let file_read = File::open(&*archive_path)?;

                    let source_bytes_read = Arc::new(AtomicU64::new(0));

                    let source_bytes_read2 = source_bytes_read.clone();
                    let prog_read = ProgressReader::new(
                        file_read,
                        move |read_len| {
                            let _ = source_bytes_read2.fetch_add(
                                read_len.try_into().expect("usize as u64"),
                                Ordering::SeqCst);
                        });

                    let zstd_decoder = zstd::stream::read::Decoder::new(prog_read)?;

                    let uncompressed_bytes_read = Arc::new(AtomicU64::new(0));
                    let uncompressed_bytes_read2 = uncompressed_bytes_read.clone();
                    let uncompressed_prog_read = ProgressReader::new(
                        zstd_decoder,
                        move |read_len| {
                            let _ = uncompressed_bytes_read2.fetch_add(
                                read_len.try_into().expect("usize as u64"),
                                Ordering::SeqCst);
                        });

                    let out_capacity = zstd::stream::read::Decoder::<'_, std::io::Empty>
                        ::recommended_output_size();
                    let zstd_bufread = BufReader::with_capacity(out_capacity,
                                                                uncompressed_prog_read);
                    let mut tar = tar::Archive::new(zstd_bufread);
                    tar.unpack(&*cmd_args.out_dir)?;

                    Ok(())
                })?;
            Ok(())
        })?;

    Ok(())
}
