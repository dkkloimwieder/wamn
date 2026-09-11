# Receiving application result — 2026-09-11

The [failed run](result.json) used `e440fb18a75560d8877c828832e9d4f021b8e21e`, and its [command](../receiving-cluster-cargo-003/record.json) exited 101 after 464.888 seconds.
The [readiness request](receiving-materializer-nodeport-readiness.json) and both HTTP mutations passed before the wait for both matching events with causation timed out.
The [first host log](failure-pod-hostgroup-default-69c5b46fd5-f87sh.log) and [second host log](failure-pod-hostgroup-default-69c5b46fd5-pvqtl.log) show native NATS authentication, the materializer startup line, a Wasm backtrace, and repeated `cannot enter component instance` failures.
These logs retain the complete observed stack, but exact function mapping remains unresolved and the later inspection, acknowledgement, advisory, telemetry, environment, and recovery checks did not execute.
All raw files and component hashes remain unchanged, with `source.json` copied as mode `0600` and all other raw files as `0664`.
