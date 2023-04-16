# ptar TODO

* Write initial README.
    * Write build instructions
    * Write bench instructions
    * Show basic benchmarks output
* Improve error handling  
  Instead of just logging errors with `tracing::error!`:
    * Send error values (from places that don't return a Result<>) to a manager thread
    * Manager thread:
        * Logs the errors
        * Possibly terminates ongoing work (by setting an AtomicBool flag then joining)
        * Increments an error_count counter.
    * Return a non-zero exit code from `main()` when error_count > 0.
