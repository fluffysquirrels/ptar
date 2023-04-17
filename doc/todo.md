# ptar TODO

* Speed up decompress
    * Try splitting each archive thread into 2: one to decompress the zstd, one to extract the tar
* ptar ThreadOffloadReader safety notes:
  > What’s to stop a deadlock with there being more than 10 buffers? It looked to me on my brief look that it might make an 11th if the consumer side was slow enough compared to the producer?

  > So yes, the offload / producer thread can make an unlimited number of buffers if the read / consumer thread is slow and not returning buffers for re-use.

  > It will block sending the 11th buffer to `ready_chunks_tx` until the read thread catches up a bit.

  > I think the only place the read thread will block in my code is in `ready_chunks_rx.recv_timeout`. If the offload thread is blocked on `ready_chunks_tx`, then `ready_chunks_rx` should have buffers ready to take immediately, and the read thread shouldn't block. So that's what prevents a deadlock I think. And as I said before, the read thread has a timeout on that receive.

* Write initial README.
    * Write build instructions
    * Write bench instructions
    * Show basic benchmarks output
