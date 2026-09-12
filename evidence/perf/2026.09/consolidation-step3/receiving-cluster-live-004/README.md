# Receiving application result — 2026-09-11

This [failed run](failure.json) used `5e7b68cc3d35bf14c3b8da5828246033ed11da71`, and its [command](../receiving-cluster-cargo-004/record.json) exited 101 after 372.380 seconds.
The [Cargo pass marker](../receiving-cluster-cargo-004/cargo.log) records completed causation, inspection-row, durable acknowledgement, and no-advisory checks, with [materializer logs](materializer-host.log) and a passing [telemetry result](telemetry/result.json).
The subsequent Rustls `CryptoProvider` panic prevented completion of the startup, environment, and operator checks, and no top-level `result.json` or `startup-burst/result.json` exists.
At `2026-09-11T20:41:21.060889Z`, the separate [post-run observation](post-run-state.json) found no owned-name matches among containers, clusters, or image tags, and the private directory was absent.
That observation is separate from automatic cleanup result capture, and all original files, modes, and the [component hash list](component-bytes.sha256) remain unchanged.
