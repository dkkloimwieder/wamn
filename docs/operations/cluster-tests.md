# Cluster tests

Use the [platform deployment test](running-tests.md#platform-and-environment-apply-on-kind) for the product platform command.
Choose an [output directory](running-tests.md#capture-a-run) and follow the [cleanup](running-tests.md#cleanup) procedure.

DEP2-02 removes the Receiving, WMS, RC, and combined identity/session cluster runners.
[Removed coverage](running-tests.md#removed-application-cluster-coverage) records the historical combinations without replacements.
[Native application tests](running-tests.md#local-application-business-tests) retain the business fixtures.

### Deployed queue recovery on wamn-dev

This case shows what the durable queue does when a host crashes during a run. It runs on the deployed wamn-dev environment, not on a kind cluster.
The run is the published WMS wiring `inventory_move_and_label` version 2. It runs no SQL. It shapes one pallet `update` event, renders a label and stores it in Cloud Storage. So nothing holds the run, and the kill must land while the run shows `running`.

The kill is a SIGKILL of the `wamn-host` process of the WMS pod, sent from a node debug pod. A forced pod delete is an operator action, so the case does not use it. This query reads the run and its queue row once a second:

```sql
SELECT r.status, r.updated_at, q.lease_generation, q.attempts, q.lease_owner, q.lease_expires_at
  FROM wamn_run.runs AS r
  LEFT JOIN wamn_run.run_queue AS q USING (env_key, run_id)
 WHERE r.run_id = '<run id>';
```

The container restarts in the same pod. The run waits for its old lease to expire. The lease lasts 30 seconds (`DEFAULT_QUEUE_LEASE_TTL_MS`).
The restarted host then claims the run again with the next lease generation, and the run completes. Its queue row is gone.
The bucket holds one label object for the run, because the killed attempt wrote no label.
The run is `completed` with no failure kind, and it needs no operator action.
