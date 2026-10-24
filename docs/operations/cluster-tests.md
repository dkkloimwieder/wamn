# Cluster tests

These commands exercise deployed application behavior.
[Cluster test limits](../testing/cluster-tests.md) explains the boundary they establish.
Choose an [output directory](running-tests.md#capture-a-run) and follow the [cleanup](running-tests.md#cleanup) procedure.
Follow [test-database isolation](running-tests.md#test-database-isolation).

Each cluster test lists the programs and environment variables that it needs in its `#[ignore = "requires: ..."]` reason.
Its first line fails and names each listed input that is missing.
A Receiving or WMS cluster case runs only the candidate release that `qualify-release` names in `WAMN_DELIVERY_CANDIDATE` ([release qualification](delivery.md#release-qualification)).

Each Receiving and WMS cluster test creates its own new result directory and prints its path.
`WAMN_RECEIVING_EVIDENCE_DIR` and `WAMN_WMS_EVIDENCE_DIR` can name an existing parent directory for it.
Without the variable, the parent is the system temporary directory, which is `$TMPDIR` or `/tmp`.
The results stay on this machine.

The app tests own decisions, provisioning calls, assertions, results, and cleanup.
The `tools/receiving-cluster-journey-run` and `tools/wms-cluster-journey-run` scripts perform only requested lifecycle actions.
Run one installation at a time when cases share fixed management ports.

### Receiving and WMS application cases

`qualify-release` runs the cases of the release's package set:

| Package set | Cases |
| --- | --- |
| `wamn_receiving` | `route_authentication_live::cluster::route_cases::command_histories` |
| `wamn_receiving`, `client_acme_receiving` | the case above and `route_authentication_live::cluster::postcommit_case::baseline_overlay_and_materializer_progress` |
| `wamn_wms` | `cluster::released_wms_routes`, `cluster::released_wms_routes_retain_committed_work_after_label_failure`, `cluster::restarted_wms_host_retains_compiled_code_and_serves_requests` |

`route_authentication_live::cluster::route_cases::user_membership_and_permission_revocation` is no qualification case, and it also runs only with a Receiving candidate.
To use the WMS routes from a browser, set `WAMN_JOURNEY_HOLD_SECONDS` on the released routes case.
With the variable set, the case keeps its route NodePort Service and serves `http://127.0.0.1:8080/`.
After the case finishes, it holds the cluster for that many seconds or until interrupted.
A passing case first prints the route and the caller token file.
Without the variable, the case does not hold.
The startup case requires one invocation and statement after restart and in steady state, without component loading or compilation.
It retains each trace and its per-span timing breakdown beside request, recovery, and cache evidence.
The cluster fixture renders the deployed collector configuration with file exporters and retains its 60-second batch timeout.
The host keeps sampled parents and 10% of new traces. The collector does not sample traces.
The tests send fixed trace IDs with sampled parent contexts, so the host keeps those traces.
The shared reader uses `kubectl exec` through a read-only sidecar to read complete OTLP JSON lines across batches.
Receiving retains its invocation identity, PostgreSQL ancestry, and scoped metric assertions.

### Environment apply on kind

`environment_apply_kind` is the kind suite of the platform.
It creates its own kind cluster `wamn-apply-<pid>` with cert-manager, CloudNativePG, the operator release and a registry with TLS and a password.
It runs `env apply`, rollback, release none and delete on two environments, and it checks the host heartbeat labels, placement by `hostSelector`, route host isolation and the operator release.
It removes the cluster and the registry at the end.
It takes the host and http role images from the release chart that `WAMN_RELEASE_CHART` names.
Build that chart first with the platform build step, against a registry at a loopback port that a container on the `kind` network publishes:

```bash
target/debug/wamn-ctl build-platform --registry localhost:<port> --out "$WAMN_RESULTS/release-chart"
WAMN_RELEASE_CHART="$WAMN_RESULTS/release-chart" \
  cargo test -p wamn-control --test environment_apply_kind -- --ignored --nocapture
```

The test prints its work directory. `report.json` in that directory records each check with its command, output, and result.

### Delivery on `env apply`

The Receiving and WMS delivery cases deliver each application's release with `wamn-ctl env apply`.
Both run on one kept kind cluster that `WAMN_DELIVERY_CLUSTER` names, `wamn-delivery-<suffix>`.
The first case creates the cluster with cert-manager, CloudNativePG, the operator release and a registry with TLS and a password. The second case attaches to it.
Each case publishes its release with its fixture, and `publish-release` pushes the release manifest.
The images come from the release chart that `WAMN_RELEASE_CHART` names, which `wamn-ctl build-platform` stamped. No case builds an image.
The case then runs `qualify-release` on the release digest and that chart, which pushes the qualification beside the release when all of its qualifying cases pass.
Then the case copies the role images by digest to the kept registry, writes `environment.k` and runs `env apply`, which reads that qualification from the registry.
It then checks the hosts and the role placement. The second case also checks the two applications together in `wamn-system`.
Run Receiving first, then WMS, then remove the cluster:

```bash
export WAMN_DELIVERY_CLUSTER=wamn-delivery-<suffix>
export WAMN_RELEASE_CHART="$WAMN_RESULTS/release-chart"
target/debug/wamn-ctl build-platform --registry localhost:<port> --out "$WAMN_RELEASE_CHART"
tools/delivery-owned receiving "$WAMN_RESULTS"
tools/delivery-owned wms "$WAMN_RESULTS"
tools/delivery-owned remove-kept "$WAMN_DELIVERY_CLUSTER"
```

A case refuses to run twice on one kept cluster, because its environment exists there.
Each case writes `delivery-checks.json`, `environment.k` and one command result per step to its results directory.

### Native RC

Build `wamn-gates`, then inspect its plan before creating the declared cluster:

```bash
"$CARGO_TARGET_DIR/debug/wamn-gates" rc
"$CARGO_TARGET_DIR/debug/wamn-gates" rc --apply --evidence-dir "$WAMN_RESULTS/rc"
```

The Rust owner uses `wamn-rc`, refuses existing resources, and runs the retained socket and trace Jobs.
It preserves exact image assertions and removes only its owned resources.
Local unit tests do not replace these deployed checks.

### Deployed queue recovery on wamn-dev

This case shows what the durable queue does when a host crashes during a run. It runs on the deployed wamn-dev environment, not on a kind cluster.
The run is the published WMS wiring `inventory_move_and_label` version 2. It runs no SQL. It shapes one pallet `update` event, renders a label and stores it in Cloud Storage. So nothing holds the run, and the kill must land while the run shows `running`.

The kill is a SIGKILL of the `wamn-host` process of the WMS pod, sent from a node debug pod. A forced pod delete is an operator action, so the case does not use it. This query reads the run and its queue row once a second:

```sql
SELECT r.status, r.updated_at, q.lease_generation, q.attempts, q.lease_owner, q.lease_expires_at
  FROM wamn_run.runs AS r
  LEFT JOIN wamn_run.run_queue AS q USING (tenant_id, run_id)
 WHERE r.run_id = '<run id>';
```

The container restarts in the same pod. The run waits for its old lease to expire. The lease lasts 30 seconds (`DEFAULT_QUEUE_LEASE_TTL_MS`).
The restarted host then claims the run again with the next lease generation, and the run completes. Its queue row is gone.
The bucket holds one label object for the run, because the killed attempt wrote no label.
The run is `completed` with no failure kind, and it needs no operator action.
