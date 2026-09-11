# Receiving test result — 2026-09-11

The [command](record.json) ran at `25bece68bfe93b3948bc2a72903ff1c3c2af806f` and exited 101 after 398.964 seconds.
The [Cargo output](cargo.log) reports one failed test, with 398.42 seconds inside the test itself.
It records `RECEIVING_MATERIALIZER_PASS` before startup returns `invalid peer certificate: UnknownIssuer`.
The [application record](../receiving-cluster-live-005/README.md) retains the passed materializer and telemetry checks and the returned startup failure.
This result does not identify the certificate trust cause or establish an outcome for later checks.
