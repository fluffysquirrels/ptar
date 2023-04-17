use crate::Error;
use crossbeam_channel::{RecvTimeoutError, TryRecvError, TrySendError};
use std::{
    collections::VecDeque,
    error::Error as StdError,
    io::{self, Read},
    result::Result as StdResult,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering}
    },
    thread,
    time::{Duration, Instant},
};

pub struct ThreadOffloadReader {
    /// Some except during drop().
    offload_thread: Option<thread::JoinHandle<()>>,
    read_timeout: Duration,
    ready_chunks_rx: crossbeam_channel::Receiver<VecDeque<u8>>,
    reuse_chunks_tx: crossbeam_channel::Sender<VecDeque<u8>>,
    curr_chunk: Option<VecDeque<u8>>,
    should_stop: Arc<AtomicBool>,
}

struct OffloadThread {
    inner: Box::<dyn Read + Send>,
    ready_chunks_tx: crossbeam_channel::Sender<VecDeque<u8>>,
    reuse_chunks_rx: crossbeam_channel::Receiver<VecDeque<u8>>,
    buf_len: usize,
    should_stop: Arc<AtomicBool>,
}

enum ThreadError {
    Error(Error),
    Shutdown,
}

type ThreadResult<T> = StdResult<T, ThreadError>;

impl ThreadOffloadReader {
    pub fn new<R: Read + Send + 'static>(inner: R) -> ThreadOffloadReader {
        let inner_boxed: Box<dyn Read + Send> = Box::new(inner);
        let (ready_chunks_tx, ready_chunks_rx) = crossbeam_channel::bounded::<VecDeque<u8>>(10);
        let (reuse_chunks_tx, reuse_chunks_rx) = crossbeam_channel::bounded::<VecDeque<u8>>(10);
        let should_stop = Arc::new(AtomicBool::new(false));

        let thread_state = OffloadThread {
            inner: inner_boxed,
            ready_chunks_tx,
            reuse_chunks_rx,
            buf_len: 128 * 1024,
            should_stop: should_stop.clone(),
        };

        let offload_thread = thread::spawn(move || OffloadThread::main(thread_state));

        ThreadOffloadReader {
            offload_thread: Some(offload_thread),
            read_timeout: Duration::from_secs(5),
            ready_chunks_rx,
            reuse_chunks_tx,
            curr_chunk: None,
            should_stop,
        }
    }
}

impl OffloadThread {
    fn main(mut self) {
        let res = (|| -> ThreadResult<()> {
            loop {
                self.check_should_stop()?;

                let mut read = 0_usize;
                let mut buf = self.empty_buf()?;
                assert_eq!(buf.len(), self.buf_len);
                let (target, _) = buf.as_mut_slices();
                assert_eq!(target.len(), self.buf_len);

                while read < self.buf_len {
                    if self.should_stop() {
                        break;
                    }

                    let count = self.inner.read(&mut target[read..])?;
                    if count == 0 {
                        break;
                    }
                    read += count;
                }

                if read == 0 {
                    break;
                }

                buf.truncate(read);
                let res = self.ready_chunks_tx.send(buf);

                if let Err(_) = res {
                    return Err(ThreadError::Shutdown);
                }
            }

            Ok(())
        })();

        match res {
            Ok(()) => (),
            Err(ThreadError::Shutdown) => (),
            Err(ThreadError::Error(err)) =>
                tracing::error!(%err, "Error in ThreadOffloadReader's offload thread"),
        };
    }

    fn empty_buf(&mut self) -> ThreadResult<VecDeque<u8>> {
        match self.reuse_chunks_rx.try_recv() {
            Ok(mut buf) => {
                buf.clear();
                buf.resize(self.buf_len, 0_u8);
                Ok(buf)
            }
            Err(TryRecvError::Empty) => {
                let mut buf = VecDeque::with_capacity(self.buf_len);
                buf.resize(self.buf_len, 0_u8);
                Ok(buf)
            },
            Err(TryRecvError::Disconnected) => Err(ThreadError::Shutdown),
        }
    }

    fn should_stop(&self) -> bool {
        self.should_stop.load(Ordering::SeqCst)
    }

    fn check_should_stop(&self) -> ThreadResult<()> {
        if self.should_stop() {
            Err(ThreadError::Shutdown)
        } else {
            Ok(())
        }
    }
}

impl<E: StdError + Send + Sync + 'static> From<E> for ThreadError {
    fn from(e: E) -> ThreadError {
        ThreadError::Error(Error::from(e))
    }
}

impl Read for ThreadOffloadReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let None = self.curr_chunk {
            let res = self.ready_chunks_rx.recv_timeout(self.read_timeout);
            let next = match res {
                Ok(buf) => buf,
                // Offload thread has terminated.
                Err(RecvTimeoutError::Disconnected) => return Ok(0),
                Err(RecvTimeoutError::Timeout) =>
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "ThreadOffloadReader::read: timeout receiving next buffer.")),
            };
            self.curr_chunk = Some(next);
        }

        let curr = self.curr_chunk.as_mut()
                       .expect("initial if statement should have returned or set self.curr_chunk");

        if curr.is_empty() {
            self.curr_chunk = None;
            return Read::read(self, buf);
        }

        // !curr.is_empty()
        let count = Read::read(curr, buf)?;
        assert!(count > 0);
        if curr.is_empty() {
            // Current buffer has been fully read, so re-use or drop it.
            let curr = self.curr_chunk.take()
                           .expect("All code paths set curr_chunk to Some or return");
            let reuse_res = self.reuse_chunks_tx.try_send(curr);
            match reuse_res {
                // Buffer re-used successfully.
                Ok(()) => (),

                // Re-use channel is full, so drop it as the offload thread is busy.
                Err(TrySendError::Full(_)) => (),

                // Offload thread's receiver is dropped, which means the offload thread
                // has terminated.
                // Next call to read() will return Ok(0), so nothing to do right now.
                Err(TrySendError::Disconnected(_)) => (),
            }
        }
        Ok(count)
    }
}

impl Drop for ThreadOffloadReader {
    fn drop(&mut self) {
        self.should_stop.store(true, Ordering::SeqCst);
        let start = Instant::now();
        let offload_thread = self.offload_thread.take()
                                 .expect("self.offload_thread() is Some(_) until now");
        while start.elapsed() < self.read_timeout {
            if offload_thread.is_finished() {
                let _ = offload_thread.join().expect(
                    "ThreadOffloadReader::drop() - joining offload thread.");
                return;
            }

            thread::sleep(Duration::from_millis(100));
        }

        // Timeout.
        tracing::error!("Timeout in ThreadOffloadReader::drop() waiting for offload thread \
                         to terminate");
    }
}
