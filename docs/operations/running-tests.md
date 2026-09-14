# Running tests

Run commands from the repository root unless a command changes directories.
[Testing strategy](../testing/strategy.md) defines what each test method establishes.
[Building](building.md) lists the toolchain and build prerequisites.

## Select the relevant tests

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

## Test-database isolation

Tests use explicitly owned disposable databases, never an interactive development database.
Test setup, execution, and cleanup must not connect to or change the interactive database.
A test database can share a PostgreSQL server only when roles and server configuration do not collide.
If a suite changes roles or server configuration, give it a separate owned PostgreSQL 18 server.
Use serial execution when cases share database state.

For a fresh local server, run `pg_virtualenv -t -v 18 bash`.
The shell receives its connection variables, and the server is removed when the shell exits.
For Docker fixtures, choose a new container name and an unused loopback port.
Make sure that a real query succeeds through the same connection path that the test uses.
Read the selected test's database-name and role requirements before setting its URL.
Exercise the intended role because a superuser bypasses row security.

The delivery test runner creates its own PostgreSQL 18 server and database.
It passes a private ownership record through `WAMN_TEST_POSTGRES_OWNERSHIP`.
It sets `WAMN_TEST_REQUIRED=1`, so selected optional constructors require their database input.
The record identifies the live process, connection port, and databases created by that runner.
The selected test constructors refuse mismatched coordinates before connecting.
The runner stops its test process group and removes its server on completion, failure, or handled interruption.

Build the runner, then give it the exact test command:

```bash
cargo build --locked --offline -p wamn-test-infrastructure --bin wamn-test-postgres
"${CARGO_TARGET_DIR:-target}/debug/wamn-test-postgres" \
  --database wamn_ctl --url-env WAMN_CTL_PG_URL -- \
  cargo test --locked --offline -p wamn-ctl --test publish_release_live \
  -- --nocapture --test-threads=1
```

The runner requires PostgreSQL 18 binaries under `/usr/lib/postgresql/18/bin`.
Repeat `--url-env` when multiple variable names must identify the same owned database.
Its Rust fixture API creates separate databases when a test needs different targets.
The runner does not accept an existing database URL.
Legacy optional tests retain their manual inputs when no ownership record is present.
This bounded adoption does not establish automatic refusal in every repository test.

## Capture a run

Ordinary tests need no result directory.
Report the command, source revision, and pass, fail, or skip outcome.
Some cluster tests and tools write diagnostic files and require a new directory.
For those commands, choose an unused path under the main checkout's `evidence/` directory:

```bash
WAMN_MAIN="$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")"
WAMN_RESULTS="$WAMN_MAIN/evidence/local-tests"
mkdir -p "$WAMN_MAIN/evidence"
mkdir "$WAMN_RESULTS"
```

Keep source unchanged while a command runs.
Use diagnostic output to understand failures and the actual execution boundary.
Keep credentials in private files and out of shared output.
No permanent archive or result-only commit is required.
[Result interpretation](../testing/evidence.md) defines passes, failures, and unexecuted cases.

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
Report each failure's package, target, full test name, and actual cause.
A changed test name does not establish a new failure or remove an earlier failure.

## Live prerequisites and troubleshooting

An exact filter can select zero tests and still return success.
For a named case, require its exact result row and one executed case.
An ignored test needs `--ignored` or `--include-ignored` after Cargo's `--` separator.
Some tests return early when an environment variable is absent and appear among reported passes.
Read their `--nocapture` output and required inputs before reporting execution.

`services/ctl/tests/support/mod.rs` owns `WAMN_CTL_PG_URL` and its cross-process database lock.
Its optional constructor permits an explicit skip and enforces an ownership record when supplied.
Its required constructor requires the owned runner above and refuses missing input or ownership.
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
For deployed runs, use [cluster commands](cluster-tests.md).

### Record history retention gate

The `record-history-retention` subcommand of `wamn-gates` tests the record history retention verb.
It needs a superuser URL to a disposable PostgreSQL 18 database, because it creates roles and replaces the catalog schemas.
Build the ctl binaries with the `ops` feature, and then build the gate binary:

```bash
cargo build --locked --offline -p wamn-ctl --features ops --bins
cargo build --locked --offline -p wamn-gates
"${CARGO_TARGET_DIR:-target}/debug/wamn-gates" record-history-retention \
  --admin-database-url "$WAMN_PG_ADMIN_URL"
```

The gate finds `wamn-ctl` and `wamn-ctl-ops` beside its own binary, or at `WAMN_CTL_BIN` and `WAMN_CTL_OPS_BIN`.
It applies a fixture package through `wamn-ctl apply-package` and mints the audit retention and run retention generations.
It prints `overall PASS: true` only when every arm passes, and a failed arm returns a nonzero status.
It drops its schemas and its generation roles when it ends.

### Local saved-edit acceptance

Use a clean linked worktree reserved for this test.
Set `SOURCE` to its absolute path.
Export `CARGO_TARGET_DIR` with the path to its separate absolute build directory.
The case changes authored files, observes the running application, and restores the saved source.
Keep port 18088 unused and run this case alone.
The fixture requires PostgreSQL 18 binaries, Docker, and the existing development service images.

Build the native programs, fixture, HTTP guest, and exact test binary from that worktree:

```bash
cd "$SOURCE"
cargo build --locked --offline \
  -p wamn-ctl -p wamn-host -p wamn-identity -p wamn-scenario-worker \
  -p wamn-test-infrastructure --bins --example delivery_timings
cargo build --locked --offline --manifest-path apps/Cargo.toml \
  --target wasm32-wasip2 -p http-route
cargo test --locked --offline -p wamn-receiving-tests --lib --no-run
```

Set `WAMN_LOCAL_TEST_BINARY` to the absolute executable path printed by the last command.
Set `WAMN_LOCAL_RESULTS` to an unused private directory outside the worktree.
Run the exact case through the owned fixture:

```bash
WAMN_LOCAL_DEV_EDIT_ROOT="$SOURCE" \
WAMN_DEV_ENV_FLOW_HTTP_COMPONENT="$CARGO_TARGET_DIR/wasm32-wasip2/debug/http_route.wasm" \
  "$CARGO_TARGET_DIR/debug/examples/delivery_timings" \
  "$SOURCE" "$CARGO_TARGET_DIR" "$WAMN_LOCAL_RESULTS" -- \
  "$WAMN_LOCAL_TEST_BINARY" \
  route_authentication_live::dev::local_delivery::local_watch_preserves_data_refuses_bad_sql_and_recreates_schema \
  --exact --ignored --nocapture --test-threads=1
```

The fixture creates its own PostgreSQL server and Compose services on assigned loopback ports.
The case refuses registry access and requires authenticated application results after code, SQL, and schema edits.
It also requires retained data for compatible edits, refusal of invalid SQL, and a new database after a schema edit.
Require one executed passing case, successful resource cleanup, and restored source before reporting success.
This correctness case does not report performance measurements.

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

Follow [test-database isolation](#test-database-isolation) before these database-backed commands.

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

`check` and `write` plan every authored and generated statement as `wamn_app` under the grants that the package declaration derives.
They do this in one transaction and roll it back, so the database keeps its roles and privileges.
If the server has no `wamn_app` role, that transaction creates it.
Connect as a role that can create roles, grant privileges, and set the role, such as the superuser of the fresh server.

If a model declares a retention other than `"none"`, create the history table of that relation after the migrations.
Do this before `check`, `write`, and SQLx prepare, so that `EXPLAIN` and SQLx prepare resolve authored SQL that names the history table.
If the server has no `wamn_app` role, `record-history.sql` grants no history read function to it.
The first command creates that role, so run it once for each server before the other two commands.
The second command installs the history table function, and the third command creates one history table:

```bash
psql -d wamn_receiving -v ON_ERROR_STOP=1 -c 'CREATE ROLE wamn_app NOLOGIN'
psql -d wamn_receiving -v ON_ERROR_STOP=1 -f deploy/sql/record-history.sql
psql -d wamn_receiving -v ON_ERROR_STOP=1 \
  -c "SELECT wamn_history.create_history_table('receiving', 'purchase_order', false)"
```

Repeat the third command for each logged relation, with its schema and relation name.
Receiving logs `purchase_order` and `purchase_order_line`, so its generation database also needs this command:

```bash
psql -d wamn_receiving -v ON_ERROR_STOP=1 \
  -c "SELECT wamn_history.create_history_table('receiving', 'purchase_order_line', false)"
```

Introspection leaves each history table out of the schema description.
Acme and WMS declare no log of their own, so their generation databases need no history table.

`check` compares the complete generated path and byte set without changing it.
For an intended declaration or SQL change, replace `check` with `write`.
Review the generated files before building the guest and operator.
Do not edit generated Rust directly.

For Acme, apply the Receiving migrations first and then its overlay migrations in a separate database.
Use `apps/client_acme_receiving` as the generation input.
For WMS, create its declared schema and apply only `apps/wamn_wms/migrations/*.sql` in its separate database.
Use `apps/wamn_wms` as the input.

SQLx uses each application's committed `tests/.sqlx/` directory during offline compilation.
After a Receiving SQL change, regenerate that cache:

```bash
RECEIVING_SQLX_DATABASE_URL="${RECEIVING_DATABASE_URL}?options=-csearch_path%3Dreceiving%2Cpublic"
(
  cd apps/wamn_receiving/tests
  CARGO_NET_OFFLINE=true cargo sqlx prepare -D "$RECEIVING_SQLX_DATABASE_URL" -- \
    --test receiving_sqlx_verifier --locked --offline
)
```

For Acme, use its separate database, `apps/client_acme_receiving/tests`, and the `client_acme_sqlx_verifier` target.
For an explicit metadata comparison, use the same command with `prepare --check`.
That comparison writes temporary output under the target and preserves committed metadata.

## Cleanup

The app and RC runners clean up their own named resources on success, failure, or handled interruption.
Inspect their recorded cleanup result and remaining resource names before reporting completion.
For manual fixtures, remove only the exact container, cluster, image, volume, and private directory that your run created.
Use explicit cluster names, kubeconfig paths, and contexts for Kubernetes and Helm commands.
Never use `docker prune`, broad image removal, or name-substring cleanup.

Report unresolved cleanup failures and keep only output needed for current work or specifically requested by the user.
Before removing a worktree, preserve its source changes and branch.
Remove only that worktree and its owned target after no process uses them.
