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

Tests never connect to or change an interactive development database.
A database test takes its PostgreSQL from the `wamn-test-postgres` crate in `test-support/postgres`, and it reads no database variable.
The crate requires the PostgreSQL 18 binaries under `/usr/lib/postgresql/18/bin`.

`wamn_test_postgres::database()` creates a database with a unique name that the calling test owns.
The first call starts one private server for the test process.
That server stops, and its directory is removed, when the test process exits, also after a panic.
Dropping the returned value drops the database.
`url()` returns its superuser URL, and `execute` runs SQL batches in it.

Roles and server settings belong to the whole server.
A test that changes roles, or depends on roles that another test of the same binary changes, holds `wamn_test_postgres::lock()` for its whole duration.
Take the lock before you create the database.

`wamn_test_postgres::start(settings)` starts a separate server, for example with `("wal_level", "logical")` or `("standard_conforming_strings", "off")`.
A test that changes the whole server, such as a control store that revokes PUBLIC CONNECT on every database, holds the lock or starts its own server.
Dropping the server stops it and removes its directory.

Floor setup functions create a database with the schema that a test starts from.
Each lives beside the crate that owns its SQL, behind that crate's `test-util` feature.
Enable the feature on a dev-dependency.
The floors do not take the lock.

| Floor | Function | Installs |
| --- | --- | --- |
| system | `wamn_control_provision::test_database::system()` | the control store, applied as `wamn_system`, which owns the database |
| tenant | `wamn_catalog::test_database::tenant()` | `CATALOG_SCHEMA_SQL` and the `wamn_app` and `wamn_scenario_author` roles |
| tenant+app_system | `wamn_project_state::test_database::tenant_app_system()` | the tenant floor and `deploy/sql/app-schema.sql` |

A test of provisioning, migrations, or grants executes that operation and does not start from a floor that already performed it.
Exercise the intended role because a superuser bypasses row security.

An ignored test needs built components, Docker, a cluster, a broker, or a registry.
It takes its database from the same functions.
When `--ignored` selects it, it fails and names its first missing input instead of skipping.

The manual generation commands below still take URL variables.
For them, run `pg_virtualenv -t -v 18 bash`.
The shell receives its connection variables, and the server is removed when the shell exits.
For Docker fixtures, choose a new container name and an unused loopback port.
Make sure that a real query succeeds through the same connection path that the command uses.

The `wamn-test-postgres` runner binary in `wamn-test-infrastructure` serves delivery tooling.
Release qualification uses it for the generation database and SQLx preparation.
It starts a server, creates the named database, and optionally applies migrations and history tables.
It removes inherited PostgreSQL variables, sets each `--url-env` variable to the database URL, and runs the command.
It stops the command's process group and removes its server on completion, failure, or handled interruption.

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
Do not infer execution from the aggregate Cargo pass count.

| Existing reference | Current owner and required setup |
| --- | --- |
| `[RUN-PLANE-RECONCILE]` | `crates/control/lib/tests/run_plane_live.rs`: runs by default on the test server and holds the process lock |
| `[CLAIMS-LIVE]` | Runtime `plugins::wamn_postgres::claims::tests` `live_*` cases: run by default on the test server, and the cases that create fixed roles hold the process lock |
| `[R18-NEG]` | Runtime `plugins::wamn_postgres::claims::tests::live_scs_off_server_fails_checkout_closed`: runs by default on a separate server started with `standard_conforming_strings=off` |
| `[EVT-READER]` | `services/cdc-reader/tests/event_reader_live.rs`: ignored, starts its own server with `wal_level=logical`, and needs `WAMN_READER_NATS_URL` |
| `[SQLX-TRANSACTION]` | `crates/platform/runtime/tests/sqlx_transaction_live.rs`: ignored, takes its database from the test server, and needs `WAMN_SQLX_TRANSACTION_COMPONENT` |
| `[MGMT-LIVE]` | `services/scenario-worker/tests/management_live.rs`: runs by default on the test server and holds the process lock |
| `[STD-GUEST-VIRTUALIZATION]` | `tests/integration/src/virtualized_std_guest.rs`: built guest files and its explicit `WAMN_STD_VIRTUALIZATION_*` inputs |
| `[EVT-C-CDC]` | `tests/integration/src/cdcbench.rs` exposes a Rust entrypoint but no `wamn-gates` subcommand |

The source owners declare additional credentials, artifact paths, and selected assertions.
Do not substitute shared services for missing test inputs.
For deployed runs, use [cluster commands](cluster-tests.md).

### Record history retention test

`services/ctl/tests/prune_record_history_live.rs` tests the record history retention verb.
It creates roles, so each case holds the process lock and starts from the tenant+app_system floor.
The `ops` feature builds `wamn-ctl-ops` beside the test binary:

```bash
cargo test --locked --offline -p wamn-ctl --features ops \
  --test prune_record_history_live -- --nocapture
```

Each case applies a fixture package through `wamn-ctl apply-package` and runs `wamn-ctl-ops prune-record-history`.
`prune_removes_only_the_expired_prefix_of_each_row` also mints the run retention generation to test the refusals.
`a_retention_change_waits_for_a_running_prune` holds the audit retention lock while the verb and a retention change wait on it.

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

The fixture starts Compose services on assigned loopback ports.
The case starts its own PostgreSQL server, because the development environment resets the control store of the whole server.
The case refuses registry access and requires authenticated application results after code, SQL, and schema edits.
It also requires retained data for compatible edits, refusal of invalid SQL, and a new database after a schema edit.
Require one executed passing case, successful resource cleanup, and restored source before reporting success.
This correctness case does not report performance measurements.

### Bounded HTTP reuse

Use the exact debug integration test binary and debug `http_request.wasm` from the intended build.
Keep source unchanged between that build and both runs.
The runner requires the local `registry:2` image.
Each test starts its own PostgreSQL server from the local PostgreSQL 18 binaries.
The runner has an open bug, Beads `wamn-1pj3`.
Replace `<hash>` below with the exact suffix from the integration test build.

```bash
export WAMN_HTTP_REUSE_TEST_BINARY="$CARGO_TARGET_DIR/debug/deps/wamn_integration_tests-<hash>"
export WAMN_HTTP_REUSE_COMPONENT_WASM="$CARGO_TARGET_DIR/wasm32-wasip2/debug/http_request.wasm"
bash tools/http-reuse-run trusted_http_route::tests::real_http_guest_reuses_connections_without_reusing_authority
bash tools/http-reuse-run trusted_http_route::tests::nested_http_authorizes_child_and_preserves_original_caller
```

Each command creates a fresh registry container, runs its exact test, and removes only owned containers and volumes.
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
`record-history-app-grants.sql` grants the history read functions to `wamn_app`, and it fails if the server has no `wamn_app` role.
The first command creates that role, so run it once for each server before the other commands.
The second command installs the history table function.
The third command grants the history read functions to `wamn_app`.
The fourth command creates one history table:

```bash
psql -d wamn_receiving -v ON_ERROR_STOP=1 -c 'CREATE ROLE wamn_app NOLOGIN'
psql -d wamn_receiving -v ON_ERROR_STOP=1 -f deploy/sql/record-history.sql
psql -d wamn_receiving -v ON_ERROR_STOP=1 -f deploy/sql/record-history-app-grants.sql
psql -d wamn_receiving -v ON_ERROR_STOP=1 \
  -c "SELECT wamn_history.create_history_table('receiving', 'purchase_order', false)"
```

Repeat the fourth command for each logged relation, with its schema and relation name.
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
