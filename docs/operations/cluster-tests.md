# Cluster tests

These commands exercise deployed application behavior.
[Cluster test limits](../testing/cluster-tests.md) explains the boundary they establish.
Choose an [output directory](running-tests.md#capture-a-run) and follow the [cleanup](running-tests.md#cleanup) procedure.
Follow [test-database isolation](running-tests.md#test-database-isolation).

Install Docker, kind, kubectl, Helm, OpenSSL, and the tools required by the selected app test.
Run from clean committed source with an isolated target directory.
Keep the parent result directory present and the selected helper directory absent.
The Rust test creates that helper directory and refuses an existing one.

The app tests own decisions, provisioning calls, assertions, results, and cleanup.
The `tools/receiving-cluster-journey-run` and `tools/wms-cluster-journey-run` scripts perform only requested lifecycle actions.
Do not call their retired `--apply` interface.
Run one installation at a time when cases share fixed management ports.

### `[RECEIVING-CLUSTER-JOURNEY]` Receiving application tests

Run the full released route, materializer, startup, recovery, and environment-isolation case:

```bash
WAMN_RECEIVING_EVIDENCE_DIR="$WAMN_RESULTS/receiving" \
  cargo test --locked --offline -p wamn-receiving-tests --lib \
  route_authentication_live::cluster::default_case::released_routes_materializer_startup_and_environment_isolation \
  -- --ignored --exact --nocapture
```

The test builds its required guests, native programs, and images.
It owns its broker credentials and provisions the declared streams before host activation.
It keeps normal TLS peer verification and separate scheduler and event credentials.

For `[RECEIVING-POSTCOMMIT]`, select the sequential baseline and additive installation comparison:

```bash
WAMN_RECEIVING_EVIDENCE_DIR="$WAMN_RESULTS/receiving-postcommit" \
  cargo test --locked --offline -p wamn-receiving-tests --lib \
  route_authentication_live::cluster::postcommit_pair::unchanged_overlay_across_baseline_and_additive_installations \
  -- --ignored --exact --nocapture
```

This case compares unchanged overlay files and guest bytes across both installations.
It retains duplicate-delivery, blocked-handler, broker-advisory, and later-valid-event assertions.
The adjacent `route_cases`, `session_cases`, and `measurement_cases` modules own the other named Receiving cases.
Select their exact test names with the same required result input.

### `[WMS-CLUSTER-JOURNEY]` WMS application tests

Run the released WMS routes:

```bash
WAMN_WMS_EVIDENCE_DIR="$WAMN_RESULTS/wms" \
  cargo test --locked --offline -p wamn-wms-tests --lib \
  cluster::released_wms_routes -- --exact --ignored --nocapture
```

For another WMS case, use a fresh result directory and replace the exact test name:

| Case | Full test name |
| --- | --- |
| Committed movement after label failure | `cluster::released_wms_routes_retain_committed_work_after_label_failure` |
| Generated terminal success and partial completion | `cluster::generated_wms_terminal_reports_success_and_partial_completion` |
| Cold, restarted, and steady requests with compiled-cache identity | `cluster::restarted_wms_host_retains_compiled_code_and_serves_requests` |
| Browser demonstration | `cluster::wms_browser_demo` |

The browser case serves `http://127.0.0.1:8080/` and keeps NodePort `30950`.
Its default hold time is 3,600 seconds. `WAMN_JOURNEY_HOLD_SECONDS` changes that duration.
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
