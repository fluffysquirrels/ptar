use crate::{ProgressReader, Result, ThreadOffloadReader};
use rayon::prelude::*;
use std::{
    fs::{self, File},
    // io::BufReader,
    path::PathBuf,
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
                        "decompress thread",
                        archive_file_name = &*archive_path.file_name()
                            .expect("archive_path.file_name().is_some()")
                            .to_string_lossy()
                    ).entered();

                    let file_read = File::open(&*archive_path)?;

                    let (source_prog_read, _source_bytes_read) = ProgressReader::new(file_read);

                    let zstd_decoder = zstd::stream::read::Decoder::new(source_prog_read)?;

                    let (uncompressed_prog_read, _uncompresed_bytes_read) =
                        ArcProgressReader::new(zstd_decoder);

                    let _out_capacity = zstd::stream::read::Decoder::<'_, std::io::Empty>
                        ::recommended_output_size();
                    // let uncompressed_bufread = BufReader::with_capacity(out_capacity,
                    //                                                     uncompressed_prog_read);

                    let uncompressed_thread_offload_read =
                        ThreadOffloadReader::new(uncompressed_prog_read);

                    let mut tar = tar::Archive::new(uncompressed_thread_offload_read);
                    // let mut tar = tar::Archive::new(uncompressed_bufread);
                    tar.unpack(&*cmd_args.out_dir)?;

                    Ok(())
                })?;
            Ok(())
        })?;

    Ok(())
}
