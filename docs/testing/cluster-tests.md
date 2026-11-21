# Cluster tests

Cluster tests exercise deployed artifacts, broker configuration, authenticated routes, and process boundaries.
A successful local invocation does not establish packaging, delivery, readiness, or shutdown behavior.
Use [running tests](../operations/running-tests.md) for prerequisites, database isolation, lifecycle commands, and owned cleanup.

## Test owners

The application Rust tests own setup decisions, provisioning calls, assertions, results, and cleanup.
Thin lifecycle entrypoints perform explicit container, cluster, and image operations.
Shared platform helpers do not own the application's business assertions.

Exercise the actual released guest and its required host services.
Observe readiness, workload identity, image identity, and environment boundaries through the selected case.
For event features, use the real broker delivery path described in [application tests](application-tests.md#committed-events).
Process and connection failures need tests when their guarantees are affected.
A simulated outcome does not establish that a real process recovers or cleans up.

## Completion

`cargo xtask test-changes --base main` lists the packages that a change selects, with their test commands.
It prints no ignored command for a package whose test targets are all `_kind.rs` files.
Run one cluster case by its exact name, as [cluster tests](../operations/cluster-tests.md) describes.
A cluster case reads each `env:` input with `std::env::var`. A missing variable fails the case with the error `environment variable not found`.
The error does not name the variable. Compare the marker of the case with the variables that you set.
The cluster test writes no result directory. Its output is the Cargo test output.
Use the case's declared finite step, retry, and timeout limits.
Inspect the runner's cleanup result after success, failure, or handled interruption.
Record unresolved owned resources as incomplete cleanup.
Keep application assertion failures distinct from setup and cleanup failures.
Report missing prerequisites, partial execution, and failures through the [test result](evidence.md).
A ready Pod alone does not establish application correctness.
