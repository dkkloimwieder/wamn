# Cluster tests

These commands exercise deployed application behavior.
[Cluster test limits](../testing/cluster-tests.md) explains the boundary they establish.
Choose an [output directory](running-tests.md#capture-a-run) and follow the [cleanup](running-tests.md#cleanup) procedure.
Follow [test-database isolation](running-tests.md#test-database-isolation).

Each cluster test lists the programs and environment variables that it needs in its `#[ignore = "requires: ..."]` reason.
Its first line fails and names each listed input that is missing.
Run from clean committed source with an isolated target directory.

Each Receiving and WMS cluster test creates its own new result directory and prints its path.
`WAMN_RECEIVING_EVIDENCE_DIR` and `WAMN_WMS_EVIDENCE_DIR` can name an existing parent directory for it.
Without the variable, the parent is the system temporary directory, which is `$TMPDIR` or `/tmp`.
The results stay on this machine.

The app tests own decisions, provisioning calls, assertions, results, and cleanup.
The `tools/receiving-cluster-journey-run` and `tools/wms-cluster-journey-run` scripts perform only requested lifecycle actions.
Do not call their retired `--apply` interface.
Run one installation at a time when cases share fixed management ports.

`tools/test-changes --cluster --name <test>` runs one case.
It runs the ignored tests whose full names contain `<test>`, one test at a time, in the packages that the change set selects.
[Select the relevant tests](running-tests.md#select-the-relevant-tests) describes that selection.
Each command below runs its case only for a change set that selects the package of that case.
If the selected packages hold no matching ignored test, the command fails and names the filter.
On an unchanged tree, add `--base <revision>` with a revision before a change to the package.
The cluster tests compile only with the `cluster` feature of their package, and `tools/test-changes --cluster` turns it on.
You can also run the case through Cargo:

```bash
cargo test --locked --offline -p wamn-receiving-tests --features cluster --lib <test> -- --ignored --test-threads=1
cargo test --locked --offline -p wamn-wms-tests --features cluster --lib <test> -- --ignored --test-threads=1
```

### `[RECEIVING-CLUSTER-JOURNEY]` Receiving application tests

Run the full released route, materializer, startup, recovery, and environment-isolation case:

```bash
tools/test-changes --cluster --name \
  route_authentication_live::cluster::default_case::released_routes_materializer_startup_and_environment_isolation
```

The test builds its required guests, native programs, and images.
It owns its broker credentials and provisions the declared streams before host activation.
It keeps normal TLS peer verification and separate scheduler and event credentials.

The same case also runs as stages on one kept cluster.
A stage that fails runs again in minutes, without a new build or cluster.
Run the setup stage first. It prints `WAMN_RECEIVING_CLUSTER=<name>` and keeps the cluster:

```bash
cargo test --locked --offline -p wamn-receiving-tests --features cluster --lib \
  route_authentication_live::cluster::stages::setup_stage -- --exact --ignored --nocapture
```

Then run the materializer, startup, and outage stages in any order, each as often as necessary:

```bash
export WAMN_RECEIVING_CLUSTER=<name>
cargo test --locked --offline -p wamn-receiving-tests --features cluster --lib \
  route_authentication_live::cluster::stages::materializer_stage -- --exact --ignored
cargo test --locked --offline -p wamn-receiving-tests --features cluster --lib \
  route_authentication_live::cluster::stages::startup_stage -- --exact --ignored
cargo test --locked --offline -p wamn-receiving-tests --features cluster --lib \
  route_authentication_live::cluster::stages::outage_stage -- --exact --ignored
```

The outage stage stops the scheduler for 150 seconds and then restores it.
It checks two states: a route answers during the outage, and every Host and release is ready again after it.

A cluster stage asserts state after a phase, never a message during it.
A message that a component writes during a phase depends on its timing, so a check of it fails at random.
For example, the released operator exits during a scheduler outage, so its heartbeat guard message appears on zero to three Hosts.
An attached stage runs the committed test code at HEAD against the images of the setup commit.
It refuses when a production path changed since the setup stage.
Remove the cluster, its containers, its private files, and its image tags with the teardown stage:

```bash
cargo test --locked --offline -p wamn-receiving-tests --features cluster --lib \
  route_authentication_live::cluster::stages::teardown_stage -- --exact --ignored
```

A commit that changes only test files keeps the host and identity images, so a new setup stage after a test fix also reuses them.

For `[RECEIVING-POSTCOMMIT]`, select the sequential baseline and additive installation comparison:

```bash
tools/test-changes --cluster --name \
  route_authentication_live::cluster::postcommit_pair::unchanged_overlay_across_baseline_and_additive_installations
```

This case compares unchanged overlay files and guest bytes across both installations.
It retains duplicate-delivery, blocked-handler, broker-advisory, and later-valid-event assertions.
The adjacent `route_cases`, `session_cases`, and `measurement_cases` modules own the other named Receiving cases.
Pass their full test names to `--name`.

The Receiving delivery case also runs Receiving through the [kind edge](../plan/web-deployment.md) in `deploy/platform/edge`.
It does so after `select-release` and `deploy-release`, so the edge serves the selected release.
The edge is one HTTPS host. It sends `/password` to identity and `/api` to the route ingress, and serves the web files from a MinIO bucket.
The case writes those files with [`wamn web upload`](deployment.md#web-client-files), so it needs `pnpm` and an installed workspace:

```bash
cargo test --locked --offline -p wamn-receiving-tests --features cluster --lib \
  route_authentication_live::cluster::delivery_case::owned_release_delivery -- --exact --ignored --nocapture
```

Through the edge, the case signs in by cookie through the edge, reads a purchase order list twice for a 304, and changes a supplier.
It writes the headers it measured to `edge-results.json` in its results directory.
To use the edge in a browser, also set `WAMN_EDGE_BY_HAND=1`.
The case then prints the edge address and writes `edge.json` with the host, the address, the certificate authority and the account.
Map the host to that address in the browser, and trust that authority.
Create `edge.done` in the results directory to end the case and remove the cluster.

### `[WMS-CLUSTER-JOURNEY]` WMS application tests

Run the released WMS routes:

```bash
tools/test-changes --cluster --name cluster::released_wms_routes
```

This name also matches the label-failure case, so the command runs both cases.
For another WMS case, replace the test name:

| Case | Full test name |
| --- | --- |
| Committed movement while the label store is missing | `cluster::released_wms_routes_retain_committed_work_after_label_failure` |
| Generated terminal move, and the label that the workflow stores | `cluster::generated_wms_terminal_reports_a_committed_move` |
| Cold, restarted, and steady requests with compiled-cache identity | `cluster::restarted_wms_host_retains_compiled_code_and_serves_requests` |

To use the WMS routes from a browser, set `WAMN_JOURNEY_HOLD_SECONDS` on the released routes case.
This exact command runs only that case:

```bash
WAMN_JOURNEY_HOLD_SECONDS=3600 cargo test --locked --offline -p wamn-wms-tests --features cluster --lib \
  cluster::released_wms_routes -- --exact --ignored --nocapture
```

With the variable set, the case keeps its route NodePort Service and serves `http://127.0.0.1:8080/`.
After the case finishes, it holds the cluster for that many seconds or until interrupted.
A passing case first prints the route and the caller token file.
Without the variable, the case does not hold.
The startup case records requests, traces, recovery, and cache identity without asserting an overhead ratio.

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

This case records what the durable queue does when a host crashes during a run. It ran on the deployed wamn-dev environment, not on a kind cluster. [Section 5.8 of the GCP page](gcp.md#58-wms-host-crash-during-a-queued-run) holds the commands. The run is the published WMS wiring `inventory_move_and_label` version 2. It runs no SQL. It shapes one pallet `update` event, renders a label and stores it in Cloud Storage. So nothing holds the run, and the kill must land while the run shows `running`. The SQL-bearing case is `interrupted_durable_queue_item_completes_after_host_restart` for a kind cluster.

The kill is a SIGKILL of the `wamn-host` process of the WMS pod, sent from a node debug pod. A forced pod delete is an operator action, so the case does not use it. This query reads the run and its queue row once a second:

```sql
SELECT r.status, r.updated_at, q.lease_generation, q.attempts, q.lease_owner, q.lease_expires_at
  FROM wamn_run.runs AS r
  LEFT JOIN wamn_run.run_queue AS q USING (tenant_id, run_id)
 WHERE r.run_id = '<run id>';
```

On 2026-09-30, at commit `5a2a0064d` with host image `wamn-host:src-91f318b6c6fe387c`, the first of up to five attempts landed inside the window. All times are UTC. The node clock and the database clock can differ by less than one second.

| Time | Event |
| --- | --- |
| 21:10:56.924 | `workflow start` writes run `3e11ecc7-68d7-41d8-b31f-45b13bb3b53b`. |
| 21:10:57.829 | The WMS host claims it: lease generation 1, lease until 21:11:27.829. |
| 21:10:58.060 | SIGKILL of the host process. The container exits with code 137. |
| 21:10:59 | The container starts again in the same pod, restart count 1. |
| 21:11:13 | The pod is `Ready`. The host was down for 15 seconds. |
| 21:11:27.870 | The restarted host claims the run again: lease generation 2, attempts 1. |
| 21:11:28.365 | The run is `completed`. Its queue row is gone. |

The run waited for its old lease to expire. The lease lasts 30 seconds (`DEFAULT_QUEUE_LEASE_TTL_MS`), and the restarted host claimed the run 41 milliseconds after the old lease expired. The run completed 30.3 seconds after the kill and 31.4 seconds after it was written. The same run without a crash took 1.2 seconds on 2026-09-29. The bucket holds one label object for the run, `gs://wamn-dev-labels/wms/82b2ffa1-142a-20e7-bc58-93751f88bd7d/1`, written at 21:11:28 by the second attempt. The bucket keeps a replaced generation for 7 days as soft-deleted, and `gcloud storage ls --soft-deleted` lists none for this object. So the killed attempt wrote no label, and the label was written once. The run is `completed` with no failure kind, and it needed no operator action.
