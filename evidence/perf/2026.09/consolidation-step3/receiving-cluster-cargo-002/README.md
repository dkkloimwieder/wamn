# Receiving test result — 2026-09-11

The [command](record.json) ran at `0bd0c089fe27a209a73ba68c1583654c1dad0e45` and exited 101 after 350.58 seconds.
The [Cargo output](cargo.log) reports one failed test after the eight P3 request cases and CDC setup passed.
The failure was `causal receipt and inspection events did not both reach EVT_4_acme_9_receiving_3_dev`.
The [saved application results](../receiving-cluster-live-002/README.md) show that readiness and both HTTP mutations passed before that wait.
Later application checks did not execute, and this result does not establish which event condition failed.
