# Receiving application result — 2026-09-11

This [run](../receiving-cluster-cargo-002/record.json) failed after 350.58 seconds at `0bd0c089fe27a209a73ba68c1583654c1dad0e45`.
The [readiness request](receiving-materializer-nodeport-readiness.json) reached the expected HTTP 404 after one refused connection, and both [order](materializer-update.json) and [receipt](materializer-receipt.json) mutations passed.
The [CDC log](cdc-reader.log) reports five published events, but the failed wait required both matching events with causation and does not establish that an inspection event was absent.
This run saved no host logs or event envelopes, and the later inspection, acknowledgement, advisory, telemetry, environment, and recovery checks did not execute.
All raw files are unchanged, including `source.json` with mode `0600` and the other files with mode `0664`, which Git does not fully retain.
