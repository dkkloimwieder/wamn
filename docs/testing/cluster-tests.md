# Cluster tests

Cluster tests exercise deployed artifacts, broker configuration, authenticated routes, and process boundaries.
A successful local invocation does not establish packaging, delivery, readiness, or shutdown behavior.
Use [running tests](../operations/running-tests.md) for prerequisites, database isolation, lifecycle commands, and owned cleanup.

## Test owners

The application Rust tests own setup decisions, provisioning calls, assertions, results, and cleanup.
Thin lifecycle entrypoints perform explicit container, cluster, and image operations.
The [Receiving cluster owner](../../apps/wamn_receiving/tests/route_authentication_live/cluster.rs) and [WMS cluster owner](../../apps/wamn_wms/tests/cluster.rs) retain their separate application cases.
Shared platform helpers do not own the application's business assertions.

Exercise the actual released guest and its required capabilities.
Observe readiness, workload identity, image identity, and environment boundaries through the selected case.
For event features, use the real broker delivery path described in [application tests](application-tests.md#committed-events).
Process and connection failures need tests when their guarantees are affected.
A simulated outcome does not establish that a real process recovers or cleans up.

## Completion

Use the case's declared finite step, retry, and timeout limits.
Inspect the runner's cleanup result after success, failure, or handled interruption.
Record unresolved owned resources as incomplete cleanup.
Keep application assertion failures distinct from setup and cleanup failures.
Report missing prerequisites, partial execution, and failures through the [test result](evidence.md).
A ready Pod alone does not establish application correctness.
