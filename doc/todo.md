# ptar TODO

* Speed up decompress
    * Try splitting each archive thread into 2: one to decompress the zstd, one to extract the tar
* Skip making the first (empty) archive. Can lazily create the archive file.
* Write initial README.
    * Write build instructions
    * Write bench instructions
    * Show basic benchmarks output
