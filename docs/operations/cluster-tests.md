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
You can also run the case through Cargo:

```bash
cargo test --locked --offline -p wamn-receiving-tests --lib <test> -- --ignored --test-threads=1
cargo test --locked --offline -p wamn-wms-tests --lib <test> -- --ignored --test-threads=1
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
cargo test --locked --offline -p wamn-receiving-tests --lib \
  route_authentication_live::cluster::stages::setup_stage -- --exact --ignored --nocapture
```

Then run the materializer, startup, and outage stages in any order, each as often as necessary:

```bash
export WAMN_RECEIVING_CLUSTER=<name>
cargo test --locked --offline -p wamn-receiving-tests --lib \
  route_authentication_live::cluster::stages::materializer_stage -- --exact --ignored
cargo test --locked --offline -p wamn-receiving-tests --lib \
  route_authentication_live::cluster::stages::startup_stage -- --exact --ignored
cargo test --locked --offline -p wamn-receiving-tests --lib \
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
cargo test --locked --offline -p wamn-receiving-tests --lib \
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

### `[WMS-CLUSTER-JOURNEY]` WMS application tests

Run the released WMS inventory routes:

```bash
cargo test --locked --offline -p wamn-wms-tests --lib \
  cluster::released_wms_routes -- --exact --ignored --nocapture --test-threads=1
```

This exact filter excludes the separate label-failure case.
The gate deploys a real session issuer and tests the direct inventory routes, business transactions, refusals, and original-result replay.
It excludes label components and wiring from the release.
For another WMS case, replace the test name:

| Case | Full test name |
| --- | --- |
| Committed movement after label failure | `cluster::released_wms_routes_retain_committed_work_after_label_failure` |
| Generated terminal inventory success | `cluster::generated_wms_terminal_reports_inventory_success` |
| Cold, restarted, and steady requests with compiled-cache identity | `cluster::restarted_wms_host_retains_compiled_code_and_serves_requests` |

To use the WMS routes from a browser, set `WAMN_JOURNEY_HOLD_SECONDS` on the released routes case.
This exact command runs only that case:

```bash
WAMN_JOURNEY_HOLD_SECONDS=3600 cargo test --locked --offline -p wamn-wms-tests --lib \
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
