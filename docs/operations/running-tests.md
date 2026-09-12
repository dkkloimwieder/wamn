# Build and test

Run commands from the repository root unless a command changes directories.
Use the [architecture overview](../exe-model.md) for ownership and contract rules.
Beads and Git record status. This document contains current commands, not run results.

## Isolate builds and retain results

Use the toolchain from `rust-toolchain.toml` and the committed lock files.
The native toolchain is Rust 1.98.0 with Clippy, rustfmt, and `wasm32-wasip2`.
Guest workspaces are `apps/Cargo.toml` and `apps/platform/no-std/Cargo.toml`.
Native builds also require the system compiler tools and `protoc`.
Keep one Cargo process per target directory.

For independent work, create a worktree and target under `$HOME/.cache/wamn-lanes`.
Do not place large build targets on a temporary memory filesystem.
Reuse your own warm target while its source and selected build profile remain appropriate.

```bash
WAMN_MAIN="$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")"
WAMN_RUN="$(date -u +%Y%m%dT%H%M%SZ)-$$"
WAMN_TREE="$HOME/.cache/wamn-lanes/test-$WAMN_RUN"
WAMN_RESULTS="$WAMN_MAIN/evidence/consolidation/$WAMN_RUN"
mkdir -p "$HOME/.cache/wamn-lanes" "$WAMN_MAIN/evidence/consolidation"
mkdir "$WAMN_RESULTS"
git worktree add --detach "$WAMN_TREE" HEAD
cd "$WAMN_TREE"
export CARGO_TARGET_DIR="$WAMN_TREE/target"
export RUSTC_WRAPPER=''
export CARGO_BUILD_JOBS=2
git rev-parse HEAD > "$WAMN_RESULTS/source.txt"
git status --porcelain=v1
```

Keep source unchanged throughout a captured build or live run.
Record the command arguments, working directory, source identity, start time, duration, exit status, and complete output.
Retain relevant artifact hashes, file modes, and cleanup observations.
Store raw captures outside the cache, under the main repository's `evidence/` directory.
Do not create dated run reports in `docs/`.

Use private directories with mode `0700` and credential files with mode `0600`.
Do not put tokens or passwords in source or published logs.
Keep commands that accept credential URLs private.
Preserve failed output before cleanup or another run.
A missing prerequisite, explicit skip, or zero selected tests is not an executed pass.

## Build

Build native programs in debug mode by default:

```bash
cargo build --locked --offline -p wamn-host -p wamn-ctl -p wamn-identity \
  -p wamn-dispatcher -p wamn-executor -p wamn-scenario-worker \
  -p wamn-cdc-reader -p wamn-gates
cargo build --locked --offline -p wamn-ctl --features ops --bin wamn-ctl-ops
```

`wamn` owns the developer commands.
`wamn-ctl-ops` owns provisioning and publication commands.
The local identity issuer needs `wamn-identity` beside the CLI or at the explicit `WAMN_IDENTITY_BINARY` path.

Build only the declared applications that the caller needs:

```bash
tools/build-components app apps/wamn_receiving
tools/build-components app apps/wamn_receiving apps/client_acme_receiving
tools/build-components all
```

These are alternative selections. The last command selects every guest from Cargo metadata.
Each selected guest gets one Cargo invocation, with the retained release profile and virtualization step.
Pass the resolved base app directories when building an overlay.
Do not combine guests into a new grouped Cargo invocation.
Cargo combines dependency features within each invocation, which can change artifact bytes.

The build tool requires `jq` and `sha256sum`.
`build-only app APP_DIRECTORY...` and `build-only all` emit an artifact plan to stdout.
`virtualize-only ARTIFACT_PLAN` refuses changed inputs or raw hashes before it updates outputs.
`watch-roots app APP_DIRECTORY...` lists selected source dependencies without building them.
For a manifest-only change, inspect `cargo metadata --no-deps` before deciding whether compilation is needed.

## Ordinary tests and lint

Run only relevant existing tests for a behavior change or a concrete failure.
For mechanical edits, compare source, paths, and syntax.
Do not repeat builds or tests without a new behavior change or concrete failure.
Cleanup boundaries do not require broad test runs.

```bash
cargo test --locked --offline -p wamn-receiving-tests --test generation
cargo test --locked --offline -p wamn-receiving-tui
cargo test --locked --offline -p wamn-wms-tests --lib
cargo test --manifest-path apps/Cargo.toml --locked --offline \
  -p wamn-receiving-data-access --all-targets
SQLX_OFFLINE=true cargo test --locked --offline \
  -p wamn-receiving-tests --test receiving_sqlx_verifier
SQLX_OFFLINE=true cargo test --locked --offline \
  -p wamn-client-acme-receiving-tests --test client_acme_sqlx_verifier
```

The application READMEs identify additional tests and binary owners.
Use `tools/repo-lint dry-run` to see its exact commands without executing them.
`tools/repo-lint run` runs the source guard, formatting, and Clippy across all three workspaces.
It reports each command and returns a nonzero status if any command fails.

## The full sweep

Workspace sweeps require an explicit user request.
Do not restart a stopped sweep or start broad runs at cleanup boundaries.
If the user requests a workspace sweep, use the retained complete command:

```bash
cargo test --workspace --locked --offline --no-fail-fast -- \
  --include-ignored --nocapture --test-threads=1 \
  --skip regenerate_checked_in_journey_schema \
  --skip regenerate_checked_in_dev_config_schema \
  > "$WAMN_RESULTS/workspace.log" 2>&1
```

Record its exit status before running another command.
Do not pipe the command through `tail` or another output filter.
`--workspace` selects all root members, including members outside Cargo's defaults.
`--no-fail-fast` retains later binary results after a failure.

`--include-ignored` selects ignored cases, but their required inputs still control whether they execute.
`--nocapture` exposes explicit skip messages. The two excluded tests write generated schemas.

If the user requests the separate contract command, run it after the sweep, including when the sweep fails:

```bash
tools/contract-diff run > "$WAMN_RESULTS/contract-diff.log" 2>&1
```

It runs the authoring contract, runtime routing contract, and guest `http-route` adversarial tests.
The guest test belongs to another workspace, so the root sweep cannot reach it.
`tools/contract-diff dry-run` only prints the commands.

Report failures, ignored cases, explicit skips, filtered cases, and executed passes separately.
Retain each failure's package, target, full test name, and actual cause.
A changed test name does not establish a new failure or remove an earlier failure.

## Live tests and fresh services

An exact filter can select zero tests and still return success.
For a named case, require its exact result row and one executed case.
An ignored test needs `--ignored` or `--include-ignored` after Cargo's `--` separator.
Some tests return early when an environment variable is absent and appear among reported passes.
Read their `--nocapture` output and required inputs before reporting execution.

Use PostgreSQL 18 on an explicitly owned disposable server.
For local PostgreSQL tests, run `pg_virtualenv -v 18 bash`.
It opens a shell against a fresh instance and removes that instance when the shell exits.
For Docker fixtures, choose a new container name and publish only an unused loopback port.
Make sure that a real query succeeds through the same connection path that the test uses.
An open TCP port alone does not establish database readiness.

Use a separate fresh server for each suite because PostgreSQL roles belong to the server.
Use `--test-threads=1` within suites that share database state.
Read each test's database-name and role prerequisites before setting its URL.
A superuser connection cannot establish tenant isolation because it bypasses row security.

`services/ctl/tests/support/mod.rs` owns `WAMN_CTL_PG_URL` and its cross-process database lock.
Its optional constructor permits an explicit skip. Its required constructor refuses missing input.
Do not infer execution from the aggregate Cargo pass count.

| Existing reference | Current owner and required setup |
| --- | --- |
| `[RUN-PLANE-RECONCILE]` | `services/ctl/tests/run_plane_live.rs`: fresh PostgreSQL 18 through `WAMN_CTL_PG_URL` |
| `[R18-NEG]` | Runtime `plugins::wamn_postgres::claims::tests::live_scs_off_server_fails_checkout_closed`: separate `WAMN_SCS_OFF_PG_URL`, server setting `standard_conforming_strings=off` |
| `[EVT-READER]` | `services/cdc-reader/tests/event_reader_live.rs`: `WAMN_READER_PG_URL` at `/postgres`, logical WAL, and `WAMN_READER_NATS_URL` |
| `[SQLX-TRANSACTION]` | `crates/platform/runtime/tests/sqlx_transaction_live.rs`: `WAMN_SQLX_TRANSACTION_PG_URL` and `WAMN_SQLX_TRANSACTION_COMPONENT` |
| `[MGMT-LIVE]` | `services/scenario-worker/tests/management_live.rs`: disposable `WAMN_PLATFORM_IDENTITY_PG_URL` and the selected case's declared inputs |
| `[STD-GUEST-VIRTUALIZATION]` | `tests/integration/src/virtualized_std_guest.rs`: built guest files and its explicit `WAMN_STD_VIRTUALIZATION_*` inputs |
| `[EVT-C-CDC]` | `tests/integration/src/cdcbench.rs` exposes a Rust entrypoint but no `wamn-gates` subcommand |

The source owners declare additional credentials, artifact paths, and selected assertions.
Do not substitute shared services for missing test inputs.
For deployment rules and retained native Job commands, read [deploy/README.md](../../deploy/README.md).

### Bounded HTTP reuse

Use the exact debug integration test binary and debug `http_request.wasm` from the intended build.
Keep source unchanged between that build and both runs.
The runner requires local `postgres:18` and `registry:2` images.
Replace `<hash>` below with the exact suffix from the integration test build.

```bash
export WAMN_HTTP_REUSE_TEST_BINARY="$CARGO_TARGET_DIR/debug/deps/wamn_integration_tests-<hash>"
export WAMN_HTTP_REUSE_COMPONENT_WASM="$CARGO_TARGET_DIR/wasm32-wasip2/debug/http_request.wasm"
bash tools/http-reuse-run trusted_http_route::tests::real_http_guest_reuses_connections_without_reusing_authority
bash tools/http-reuse-run trusted_http_route::tests::nested_http_authorizes_child_and_preserves_original_caller
```

Each command creates fresh PostgreSQL and registry containers, runs its exact test, and removes only owned containers and volumes.
These cases test connection reuse and authority isolation without measuring throughput.

## Application generation and SQLx

Generate each application against its own migrated database.
Use a base-only database for Receiving and a separate base-plus-overlay database for Acme.
Otherwise, introspection can write overlay fields into generated base files.
Use another database for WMS.

Inside a fresh PostgreSQL 18 shell, prepare Receiving from its migrations:

```bash
createdb wamn_receiving
psql -d wamn_receiving -v ON_ERROR_STOP=1 -c 'CREATE SCHEMA receiving'
for migration in apps/wamn_receiving/migrations/*.sql; do
  psql -d wamn_receiving -v ON_ERROR_STOP=1 -f "$migration" || exit
done
RECEIVING_DATABASE_URL="postgresql://$PGUSER:$PGPASSWORD@127.0.0.1:$PGPORT/wamn_receiving"
WAMN_SCHEMA_INTROSPECTION_PG_URL="$RECEIVING_DATABASE_URL" \
  cargo run --locked --offline -p wamn-schema-generator --example materialize_package \
  -- check apps/wamn_receiving
```

`check` compares the complete generated path and byte set without changing it.
For an intended declaration or SQL change, replace `check` with `write`, then run `check` again.
Review the generated files before building the guest and operator.
Do not edit generated Rust directly.

For Acme, apply the Receiving migrations first and then its overlay migrations in a separate database.
Use `apps/client_acme_receiving` as the generation input.
For WMS, create its declared schema and apply only `apps/wamn_wms/migrations/*.sql` in its separate database.
Use `apps/wamn_wms` as the input.

SQLx uses each application's committed `tests/.sqlx/` directory during offline compilation.
After a Receiving SQL change, regenerate and compare that cache:

```bash
RECEIVING_SQLX_DATABASE_URL="${RECEIVING_DATABASE_URL}?options=-csearch_path%3Dreceiving%2Cpublic"
(
  cd apps/wamn_receiving/tests
  CARGO_NET_OFFLINE=true cargo sqlx prepare -D "$RECEIVING_SQLX_DATABASE_URL" -- \
    --test receiving_sqlx_verifier --locked --offline
  CARGO_NET_OFFLINE=true cargo sqlx prepare --check -D "$RECEIVING_SQLX_DATABASE_URL" -- \
    --test receiving_sqlx_verifier --locked --offline
)
```

For Acme, use its separate database, `apps/client_acme_receiving/tests`, and the `client_acme_sqlx_verifier` target.
The `--check` command writes temporary output under the target directory and preserves committed metadata.

## Disposable application clusters

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

## `[WAMN-DEV-ENVIRONMENT]` Developer session

Build `wamn`, `wamn-host`, `wamn-scenario-worker`, and `wamn-identity` before starting a session.
Use only disposable PostgreSQL, scheduler NATS, event NATS, registry, and telemetry services.
The system administrator URL must name `wamn_system` without a query or fragment.
`wamn dev up` resets its control store, so shared or durable targets are unsuitable.

Read the current required arguments from the existing command:

```bash
"$CARGO_TARGET_DIR/debug/wamn" dev up --help
```

Supply an explicit environment directory, package roots, built binaries, endpoints, artifact references, and registry authentication file.
The gate listener needs a fixed, unused port rather than port zero.
Supply event runtime credentials separately from provisioning credentials.
Declare `--stream-replicas` and `--dup-window-secs` explicitly.
Use `--event-provisioning-username` and `--event-provisioning-password-file` for stream creation.
Use `--event-nats-username` and `--event-nats-password-file` for runtime access.

The command writes private `dev.json` and prints the next developer command.
Keep `wamn dev up` running in its terminal.
From a second terminal, use its emitted configuration:

```bash
"$CARGO_TARGET_DIR/debug/wamn" dev --config "$WAMN_DEV_ENV_DIR/dev.json" \
  --overlay-root "$PWD/apps/client_acme_receiving" --watch --tui receiving
```

For `[RECEIVING-TUI]` and `[GENERATED-TUI]`, `--tui receiving` opens the app-owned operator.
Bare `--tui` opens the developer console. Without a terminal, `--hold` keeps the activation alive until interruption.
The loop supplies the route, target instance, and private personal access token.
Do not copy that token into command arguments.

The watch loop requires local Git references and a reflog.
Make sure that `git config core.logAllRefUpdates` reports `true`.
After changing generator code, rebuild `wamn` and restart the developer process.

The local Receiving terminal test uses an HTTP fixture and requires its built operator:

```bash
cargo build --locked --offline -p wamn-receiving-tui
python3 apps/wamn_receiving/tests/operator_pty.py --binary "$CARGO_TARGET_DIR/debug/wamn-receiving"
```

## `[GUEST-DIGEST-REPRODUCIBILITY]` Guest artifact comparisons

Use two clean worktrees at the same commit to test checkout independence.
Give each worktree its own target and empty `RUSTC_WRAPPER`.
Run `tools/build-components all` in each worktree.
Then pass their `target/virtualized/std-empty-environment` paths to the existing comparison:

```bash
WAMN_DIGEST_REPRO_A="$GUEST_REPRO_FIRST_ARTIFACTS" \
WAMN_DIGEST_REPRO_B="$GUEST_REPRO_SECOND_ARTIFACTS" \
  cargo test --locked --offline -p wamn-conformance-tests --test guest_workspace_closure \
  one_commit_built_in_two_checkouts_yields_identical_guest_digests \
  -- --include-ignored --exact --nocapture
```

To compare app and full selections, build the same commit into separate targets:

```bash
CARGO_TARGET_DIR="$WAMN_TREE/app-target" RUSTC_WRAPPER='' \
  tools/build-components build-only app apps/wamn_receiving > "$WAMN_RESULTS/app-plan.json"
CARGO_TARGET_DIR="$WAMN_TREE/all-target" RUSTC_WRAPPER='' \
  tools/build-components build-only all > "$WAMN_RESULTS/all-plan.json"
WAMN_DIGEST_PROFILE_APP_PLAN="$WAMN_RESULTS/app-plan.json" \
WAMN_DIGEST_PROFILE_ALL_PLAN="$WAMN_RESULTS/all-plan.json" \
  cargo test --locked --offline -p wamn-conformance-tests --test guest_workspace_closure \
  one_commit_built_under_two_profiles_yields_identical_guest_digests \
  -- --include-ignored --exact --nocapture
```

Retain the plans and comparison output before changing digest pins.
Use these comparisons when guest dependencies, selected features, workspace membership, or build flags change.

### `[AGENT-PILOT]` Authoring experiment

The pilot measures package authoring from declared task inputs.
Read the [protocol](agent-pilot.md) before running or grading it.
`tests/integration/src/agent_pilot` owns preparation, grading, interpretation, and cleanup.

```bash
cargo build --locked --offline -p wamn-gates --bin wamn-gates
cargo test --locked --offline -p wamn-integration-tests --lib agent_pilot:: -- --nocapture
tools/agent-pilot-run all --run 001 --agent claude \
  --task tests/integration/fixtures/agent-pilot/tasks/dock-appointments
```

Choose an unused run identifier. Run only one pilot at a time, separately from cluster tests.
The runner refuses occupied ports `54332`, `5004`, `4224`, `3201`, `4319`, and `8088`.
`CARGO_TARGET_DIR` selects the built native entrypoint, with `target/debug/wamn-gates` as the fallback.
The separate actions are `up`, `launch`, `grade`, and `down`.
`--agent stub` selects the local driver, without completing or grading a Dock implementation.

The runner builds its prepared source into a separate target and records source comparisons and binary hashes.
Keep that source unchanged until `up` succeeds.
Preparation removes grading inputs and this marked section from the measured worktree.
The measured agent cannot push or use `bd` through its supplied environment.

Run data initially lives under `${XDG_CACHE_HOME:-$HOME/.cache}/wamn-pilot/runs`.
Export the run before reclaiming that cache:

```bash
tools/agent-pilot-report --run 001
tools/agent-pilot-run down --run 001
```

The exporter requires completed human inputs and refuses credentials that survive its scrubber.
It writes retained output under `evidence/experiments/agent-authoring/`.
Cleanup preserves unexported run data and targets still used by another run.
Repeated `down` is safe.

For a recorded run, `tools/agent-pilot-grade --replay RUN_DIRECTORY` preserves its original grading files.
`--placement` and `--contract` also take an explicit recorded run directory.
Missing request logs cannot establish replayed execution.

## Cleanup and protected resources

The `wamn` kind cluster and its `kind-wamn` context are frozen.
Do not restart or replace its PostgreSQL, NATS, `wamn-pg`, or `wamn-sysdb` resources.
The PostgreSQL fixture uses temporary pod storage, so a restart can destroy its data.

The app and RC runners clean up their own named resources on success, failure, or handled interruption.
Inspect their recorded cleanup result and remaining resource names before reporting completion.
For manual fixtures, remove only the exact container, cluster, image, volume, and private directory that your run created.
Use explicit cluster names, kubeconfig paths, and contexts for Kubernetes and Helm commands.
Never use `docker prune`, broad image removal, or name-substring cleanup.

Keep failed output and unresolved owned resources until their cause is understood.
Before removing a worktree, preserve its branch, raw captures, and required artifacts outside its cache.
Remove only that worktree and its owned target after no process uses them.
