# Receiving application result — 2026-09-11

This [failed run](result.json) used `25bece68bfe93b3948bc2a72903ff1c3c2af806f`, and its [command](../receiving-cluster-cargo-005/record.json) exited 101 after 398.964 seconds.
The [Cargo pass marker](../receiving-cluster-cargo-005/cargo.log) records completed materializer causation, inspection-row, durable acknowledgement, and no-advisory checks, followed by a passing [telemetry result](telemetry/result.json).
Startup then returned `invalid peer certificate: UnknownIssuer`, which appears in both the [startup result](startup-burst/result.json) and top-level result.
The startup record shows cleanup signals sent to two local processes without exceeding the grace period, with both numeric exit codes absent.
The later environment and operator checks did not run, and every original file, mode, and [component hash](component-bytes.sha256) remains unchanged.
