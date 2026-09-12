The [recovered host build](host-build-inspect.stdout) `i16rmem417dudhs35c9c2tlvk` completed all 28 steps in 7 minutes 4 seconds at source `25bece68bfe93b3948bc2a72903ff1c3c2af806f`.
The [gates build](build-inspect.stdout) `y1wez2ts5mo1oqctk712iv2bf` failed after 8.1 seconds, and its component build used `cargo +1.97.0`.
The [linker output](build-log.stderr#L124) shows the CLI 0.3 async exports, then reports `invalid leading byte (0x2) for import name (at offset 0xe45)` while decoding the materializer's component metadata.
The earlier [successful standalone build](../../native-c-materializer-async/cli-001/build-result.json) used Rust 1.98.0, but these records do not establish that a corrected Docker build passes.
The [post-run observation](post-run-observation.json) records five successful read-only commands and absence of the exact owned resources and private directory at 21:20:59.081130 UTC, while the [original retrieval commands](commands.json) retain their separate capture times.
