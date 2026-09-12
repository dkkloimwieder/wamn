# WMS startup test run

The ordinary and live WMS startup commands passed at `fb6c2b2e28fbcb90a8a373672a60e2397488cae5`.
The ordinary command passed all three tests, with 22 filtered cases.
It took 76.407 seconds, including 4.10 seconds of test execution.
The live command passed its one test, with 24 filtered cases.
It took 447.509 seconds, including 446.77 seconds of test execution.

The [ordinary command](ordinary-command.json) and [live command](live-command.json) used Rust 1.98.0 and the assigned worktree's own target directory.
Both commands used host permissions for local processes and the owned services.
The [ordinary log](ordinary-cargo.log) and [live log](live-cargo.log) retain all output.
The records include the test binary hash and the actual environment overrides.

The [source comparison](source-stability-live.json) found no changed HEAD, bytes, or modes across 29,110 tracked files outside Beads.
The [compressed source maps](source-archives.json) retain the full file comparison.
The [post-run observation](post-run-observation.json) found no remaining owned container, cluster, image, or private directory.
Its three read-only commands exited zero at 2026-09-12T00:31:40.440960Z.

The [application result](../wms-startup-live-002/README.md) describes the retained assertions.
The [publication map](publication.json) records every original file hash, byte count, and POSIX mode.
The original files remain in the main repository until integration compares them.
