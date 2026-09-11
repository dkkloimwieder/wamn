Both offline SQLx tests pass at `b8f960670feb5929914ec563df16c732d8b2a642`.
Receiving executes `native_verifier_compiles_the_exact_runtime_sql_files` in 2.337392108 seconds.
Acme executes `client_acme_native_verifier_compiles_the_exact_runtime_sql_files` in 34.897328781 seconds.
Each command exits 0 with one passed test, no failures, and no ignored or filtered tests.
These elapsed times include compilation and process startup.

[Raw results](results.json) retain the commands, source, environment, exit codes, and elapsed times.
[Receiving output](command-1.log) and [Acme output](command-2.log) retain the complete compiler and test output, including warnings.
The commands use Rust 1.98.0, debug builds, two jobs, an empty `RUSTC_WRAPPER`, and `SQLX_OFFLINE=true`.
This run checks the two native SQLx targets against committed metadata.
It does not run a database, guest, cluster, or full workspace test.

[The split comparison](preparation/split-comparison.json) records unchanged module and test bodies, both function names, and all 28 query macros.
[The cache comparison](preparation/cache-moves.json) records the 27 unchanged files and their modes, with 21 owned by Receiving and six by Acme.
The split integrates as `a2e0b2fb`, and the cache moves integrate as `b8f96067`.
[The publication record](publication.json) compares their source blobs and records the hashes and modes of the original run files.

[The metadata command](preparation/metadata-command.json) records the separate offline Cargo metadata step during extraction.
Its [compressed output](preparation/metadata.json.gz) supports the Cargo workspace and target structure, not the test result.
[Compression details](preparation/metadata-compression.json) record the original byte count and SHA-256 hash, with an exact decompression comparison.
