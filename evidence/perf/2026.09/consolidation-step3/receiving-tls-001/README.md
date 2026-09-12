# Receiving TLS test result — 2026-09-11

The [command](record.json) ran at `25bece68bfe93b3948bc2a72903ff1c3c2af806f` and exited 0 after 71.841 seconds.
The [Cargo output](cargo.log) records one passed test, no failures, and no ignored tests.
The test enters the existing startup wrapper and builds a Rustls client configuration with an empty root store and no client authentication.
It checks provider initialization after the [retained panic](../receiving-cluster-live-004/README.md), and does not execute a cluster or materializer case.
