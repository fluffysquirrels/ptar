use std::{
    io::{self, Read},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub struct ArcProgressReader<R: Read> {
    bytes_read: Arc<AtomicU64>,
    prog_read: progress_streams::ProgressReader<R, Box<dyn FnMut(usize) + Send>>,
}

impl<R: Read> ArcProgressReader<R> {
    pub fn new(inner: R) -> ArcProgressReader<R> {
        let bytes_read = Arc::new(AtomicU64::new(0));
        let bytes_read2 = bytes_read.clone();
        let prog_read = progress_streams::ProgressReader::new(
            inner,
            Box::new(move |read_len: usize| -> () {
                let _ = bytes_read2.fetch_add(
                    read_len.try_into().expect("usize as u64"),
                    Ordering::SeqCst);
            }) as Box<dyn FnMut(usize) + Send>);
        ArcProgressReader {
            bytes_read,
            prog_read,
        }
    }

    pub fn bytes_read(&self) -> Arc<AtomicU64> {
        self.bytes_read.clone()
    }
}

impl<R: Read> Read for ArcProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.prog_read.read(buf)
    }
}
