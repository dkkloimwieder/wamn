# Receiving test result — 2026-09-11

The [command](record.json) ran at `5e7b68cc3d35bf14c3b8da5828246033ed11da71` and exited 101 after 372.380 seconds.
The [Cargo output](cargo.log) reports one failed test, with 289.99 seconds inside the test itself.
It records `RECEIVING_MATERIALIZER_PASS` before Rustls panics because it cannot select the process-level `CryptoProvider` from the enabled features.
The [application record](../receiving-cluster-live-004/README.md) retains the passed materializer and telemetry checks alongside this failure.
The test did not complete its remaining checks or write a final result on unwind.
