# Receiving test result — 2026-09-11

The [command](record.json) ran at `e440fb18a75560d8877c828832e9d4f021b8e21e` and exited 101 after 464.888 seconds.
The [Cargo output](cargo.log) reports one failed test, with 382.42 seconds inside the test itself.
The failure was `causal receipt and inspection events did not both reach EVT_4_acme_9_receiving_3_dev`.
The [application record](../receiving-cluster-live-003/README.md) retains the host logs captured before cleanup and the checks reached before this timeout.
This run establishes no result for the later application checks.
