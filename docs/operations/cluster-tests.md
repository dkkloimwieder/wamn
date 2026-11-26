# Cluster tests

No test target installs the platform or applies an environment on a kind cluster.
[Platform and environment apply on kind](running-tests.md#platform-and-environment-apply-on-kind) gives the current state and the manual procedure.
The one cluster target, `wamn-journey-tests --test app_routes_kind`, runs against an installed platform.
[Development on kind](running-tests.md#development-on-kind) gives its inputs and command.
Epic 14 brings the journey tests on kind.

Choose an [output directory](running-tests.md#capture-a-run) and follow the [cleanup](running-tests.md#cleanup) procedure.
The Receiving, WMS, RC and combined identity and session cluster runners are removed.
[Removed coverage](running-tests.md#removed-application-cluster-coverage) lists the combinations that have no replacement.
[Application business tests](running-tests.md#local-application-business-tests) keep their assertions and need product deployment fixtures.

### Deployed queue recovery on wamn-dev

This manual case shows what the durable queue does after a host stops during a run.
It runs on the deployed wamn-dev environment, not on a kind cluster.
The run is the published WMS workflow graph `inventory_move_and_label` version 2. It runs no SQL.
It shapes one pallet `update` event, renders a label and stores it in Cloud Storage.
So nothing holds the run, and the kill must occur while the run shows `running`.

The kill is a SIGKILL of the `wamn-host` process of the WMS pod, sent from a node debug pod.
A forced pod delete is an operator action, so the case does not use it.
This query reads the run and its queue row once a second:

```sql
SELECT r.status, r.updated_at, q.lease_generation, q.attempts, q.lease_owner, q.lease_expires_at
  FROM wamn_run.run AS r
  LEFT JOIN wamn_run.run_queue AS q USING (run_id)
 WHERE r.run_id = '<run id>';
```

The container restarts in the same pod. The run waits for its old lease to expire.
The lease lasts 30 seconds (`LEASE_TTL` in `services/host/src/host/workflow.rs`).
The restarted host then leases the run again with the next lease generation, and the run completes.
Its queue row is gone.
The bucket holds one label object for the run, because the killed attempt wrote no label.
The run is `completed` with no failure type, and it needs no operator action.
