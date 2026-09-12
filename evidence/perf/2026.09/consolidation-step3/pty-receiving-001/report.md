# Receiving terminal test

The existing app-owned terminal test passed with exit 0 in 4.860752706998028 seconds. [result.json](result.json) records source `47391bbc237f2e760f5cddb8b2e8f54a417ccc1f`, the exact command, binary hash and times. [run.log](run.log) preserves the complete output.

The driver exercised successful startup, pending-request barriers, target replacement, SIGTERM, SIGINT and Ctrl-C. It also checked HTTP bytes, terminal restoration and retained credential assertions. This was one existing driver invocation, with no skipped arm.

All 1,884 captured tracked files retained their bytes and modes. The capture excluded `.beads/` and `docs/perf/`. The Git source revision and binary also remained unchanged. The original Git status records retain the unrelated files that already existed.

The command ran a local HTTP fixture and the supplied Receiving binary. It ran no Cargo command, container, cluster, external agent or private pilot operation. This run did not change a field or exercise `check_descriptors`, which the separate live caller invokes. [publication.json](publication.json) records comparisons for all eight original capture files.
