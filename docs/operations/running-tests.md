# Running tests

Run commands from the repository root unless a command changes directories.
[Testing strategy](../testing/strategy.md) defines what each test method establishes.
[Building](building.md) lists the toolchain and build prerequisites.

Before a test run, prepare the checkout once from its repository root:

```bash
eval "$(tools/lane-start "$PWD")"
```

The script prepares submodules, native tools, and every package.
It exports `CARGO_TARGET_DIR` and `WAMN_CHECKOUT_KEY`.
Cargo output lies in `$CARGO_TARGET_DIR`, so the commands below use that path and not `target/` or `apps/target`.
`wamn build` still writes package output to `apps/target/wamn`.
The script needs Docker and the [build prerequisites](building.md), including installed web dependencies.

## Select the relevant tests

Run only relevant existing tests for a behavior change or a concrete failure.
For mechanical edits, compare source, paths, and syntax.
Do not repeat builds or tests without a new behavior change or concrete failure.
Cleanup boundaries do not require broad test runs.

`tools/test-changes` selects suites and builds their inputs before tests run.
The change set is the working tree against `git merge-base HEAD main`, or against the commit that `--base REF` names.
It includes committed, staged, unstaged, deleted, and untracked files.

- A file in a Cargo package selects that package and its dependents in every workspace that resolves it: normal and build dependents transitively, then one development dependent.
- A file under `apps/<name>/` outside a package selects every package under that directory in every workspace, with their dependents.
- A workspace `Cargo.toml` or `Cargo.lock` runs that whole workspace.
- Canonical router/runtime WIT changes select all three workspaces because direct bindings cross Cargo dependency boundaries. Shared router-delivery and materializer WASI WIT select root and apps.
- Any other file outside `docs/`, `.beads/`, and the root Markdown files runs the root command `cargo test --workspace --locked --offline --features wamn-control/ops,wamn-ctl/ops --no-fail-fast`.
- Documentation, `.beads/`, and root Markdown changes select the quality suite.
  A Markdown file below another directory also follows the package rules above.

Selected packages run their default tests with the required features of their targets.
The tool builds the guests that selected suites need.
`tools/repo-lint` also compiles guest targets that disable Cargo tests.
A passing workspace test result does not establish compilation of those guest targets.
If the change set selects a guest of the host image, `tools/test-changes` also runs `docker build --target component-builder --build-arg WAMN_CHECKOUT_KEY="$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)"`.
These guests are the packages that the Dockerfile `component-builder` stage builds, so a shared WIT root or a platform crate that a guest links selects them.
The stage checks the router pin, as the guest build of `wamn build` does.
Control-client tests compose this pinned raw guest with an empty dispatcher and link operation calls without authority.
`tools/test-changes --cluster` runs no guest build.
The path rules in `tools/test-changes-suites.json` add suites beyond Cargo dependencies.
Generator changes select `package_digest` in `wamn-control`.
Platform fixture and catalog changes select `wamn-control-provision`.
SQL changes select `wamn-ctl` with `ops`, including `protected_relations_live`.
Control and platform deployment changes select prekind rendering and the exact native host case.
The prekind command uses one `prekind` filter across both test targets.
It does not use `--skip kind`, which also skips prekind cases.
Web changes select repository web lint, runtime tests, and platform UI checks, tests, and build.
Every change selects `tools/repo-lint run`, including root formatting and Clippy for every target.

The tool builds native binaries, application packages, and guests only when selected suites need them.
It builds HTTP route bytes in release mode and materializer bytes in debug mode under `$CARGO_TARGET_DIR/apps`.
It builds the three HTTP workflow guests under `$CARGO_TARGET_DIR/no-std`.
It copies those debug guests to the root target path that the native host test reads.
It builds applications through a private PostgreSQL database.
The quality suite needs generated application files because Clippy compiles every target.
The tool derives those application roots from Cargo metadata and builds each base before its overlays.
It exports `WAMN_APPLICATION_COMPONENTS` and the selected guest and host paths.
Control and CLI suites enable `ops`.
Runtime changes also select the ignored JetStream round trip.
The tool starts a private NATS server on a free loopback port and stops it at exit.
Set `WAMN_NATIVE_C_NATS_BIN` to a prebuilt `nats-server`, or put `nats-server` on `PATH`.
If neither exists, the tool fails and names the variable.
It never downloads or compiles NATS.

Other cross-package file readers still need explicit path rules.
`tools/test-changes dry-run` prints the selected commands.
`tools/test-changes run` reports each step and returns a nonzero status if any command fails.
Each required command must report at least one executed passing case and exit successfully.
The same rule applies to contract-diff, owned delivery, and the upstream release tests.
`tools/require-test-result` captures command output in a private temporary file, prints it after completion, and removes the file.
It preserves command failures and refuses absent or entirely ignored test results.
An explicit dry run or a change set with no selected tests remains a planning result.
It records no permanent result.
`tools/test-changes` is the broad check before integration.

`tools/train-run REF [--base BASE]` tests a committed revision in a fresh detached worktree at `/var/tmp/wamn-train/worktree`.
Its default base is the merge base of `REF` and `main`.
If that worktree already exists, the tool refuses and names its path.
It initializes submodules and reuses `$HOME/.cache/wamn-target/train` between trains.
The fixed worktree path keeps compiled source paths valid between trains.
It stores logs in a temporary directory under `/var/tmp`.
If the selected suites need web packages, it runs `pnpm install --offline` there.
It then runs `tools/test-changes run --base BASE` in that worktree.
It prints a PASS or FAIL table with UTC start and end times for each step.
After success, it removes the worktree and logs but keeps the Cargo target directory.
After failure, it prints the retained worktree path and keeps its logs and build files.
It uses committed files from `REF`, so commit tool changes before testing them through this runner.
Run `tools/train-tools-test` for offline self-tests with fake external commands.

`tools/test-changes --cluster` runs the [ignored tests](#ignored-tests) of the selected packages instead of their default tests.
It uses only the root workspace.
It adds `-- --ignored --test-threads=1` to the root workspace command, so the tests run one at a time.
The generated exclusion tests in `apps/Cargo.toml` need PostgreSQL diagnostics, so each runs through its own example.
The `exclusion_diagnostics` example of `wamn-test-infrastructure` runs the platform fixture test, and the `exclusion_diagnostics` example of `wamn-receiving-tests` runs the Receiving test.
[Ignored tests](#ignored-tests) gives the commands.
`--name FILTER` is valid only with `--cluster`.
The tool passes the filter to Cargo as the test name filter, which selects the tests whose full names contain it.
The filter matches only tests in the packages that the change set selects.
If the change set selects no package, `run` and `dry-run` fail and name the filter.
Otherwise, before `run` starts the tests, the tool lists the ignored tests that match the filter.
If no ignored test in the selected packages matches, the tool fails and names the filter.
`dry-run` compiles nothing, so it does not list the tests.

To run a named ignored test on an unchanged tree, add `--base <revision>` with a revision before a change to its package.
You can also run the test through Cargo in its package:

```bash
cargo test --locked --offline -p <package> <test> -- --ignored --test-threads=1
```

The following commands select individual test targets:

```bash
cargo test --locked --offline -p wamn-schema-generator --test platform_generation
cargo test --locked --offline -p wamn-wms-tests --lib
cargo test --manifest-path apps/Cargo.toml --locked --offline \
  -p wamn-receiving-data-access --all-targets
```

The application READMEs identify additional tests and binary owners.
Use `tools/repo-lint dry-run` to see its exact commands without executing them.
`tools/repo-lint run` runs the source guard, formatting, and Clippy across all three workspaces, and then the repository policy lints of the `repo-policy` binary.
It reports each command and returns a nonzero status if any command fails.
The policy lints reject duplicate numeric prefixes among `NNNN_[a-z0-9_]+.sql` files within each directory under `deploy/sql/migrations`.

Before integration, the train runs `cargo run -p wamn-submodule-check -- main` with Git history and initialized submodules.
Changed pins require a commit subject that names the submodule path or its last segment and says `bump` or `pin`, plus `revert` when the pin does not descend from main.

## What a test asserts

A test stays only if a realistic defect fails it, and nothing else rejects that defect more strongly.
The expected value of the test must not come from the code under test.

- No test compares source text, DDL text, or a generated file to a string.
- A byte or digest test stays only when the bytes are a persisted or wire contract and the expected value is independent.
- A source or repository policy check is a lint in `tools/repo-lint`, not a test.
- A schema rule is tested against the installed database: a refused write, a cascade, or a `pg_catalog` fact.
- A new live test must not repeat an existing one. Extend the existing test when a rule has no other check.

## Local application business tests

Receiving command histories and WMS operation/replay assertions use real components, the HTTP shell, production authorization, and disposable PostgreSQL.
The fixture composes the pinned HTTP entry with the application and binds operation authority from its format-11 manifest.
They share the in-process runtime setup in `tests/integration/src/local_application.rs`.
The fixture starts a private scheduler NATS with TLS and replicates the host auth cache from its `wamn-auth` bucket.
Set `WAMN_NATIVE_C_NATS_BIN` to a local `nats-server` executable.
These tests need no registry, cluster, gate binary, or container image.
Deployment tests retain startup, restart, queue, CDC/materializer, and label delivery assertions.

Build the packages that these tests use, with a PostgreSQL server URL in `DATABASE_URL` ([building](building.md#wamn-build)), and the route guest:

```bash
"$CARGO_TARGET_DIR/debug/wamn" build apps/wamn_receiving apps/client_acme_receiving apps/wamn_wms
RUSTFLAGS="$(tools/guest-rustflags)" cargo build --manifest-path apps/Cargo.toml --locked --offline \
  --target wasm32-wasip2 --release -p http-route
```

The build composes Acme with the Receiving base into one component.
It writes the virtualized application components to `$CARGO_TARGET_DIR/virtualized/std-empty-environment`.

Run the local business assertions:

```bash
WAMN_APPLICATION_COMPONENTS="$CARGO_TARGET_DIR/virtualized/std-empty-environment" \
WAMN_FLOW_HTTP_COMPONENT="$CARGO_TARGET_DIR/wasm32-wasip2/release/http_route.wasm" \
WAMN_NATIVE_C_NATS_BIN="$(command -v nats-server)" \
  cargo test --locked --offline -p wamn-receiving-tests -p wamn-wms-tests \
    --lib local_business:: -- --ignored --test-threads=1
```

The publish composition tests use the pinned `http-route` guest from the build above.
They inspect composed bytes and SQL ownership without serving requests or starting a database:

```bash
WAMN_FLOW_HTTP_COMPONENT="$CARGO_TARGET_DIR/wasm32-wasip2/release/http_route.wasm" \
  cargo test --offline -p wamn-control --test application_composition -- --include-ignored
```

The edge bundle and release digest tests use the same pinned guest.
Build `apps/edge_device` first, with `DATABASE_URL` set as [building](building.md#wamn-build) describes.
Both targets are ignored by default because they need built components.
The release publication case also needs Docker.
It starts a private registry and passes its address as `WAMN_COMPONENT_ARTIFACT_BASE` to a child process.

```bash
"$CARGO_TARGET_DIR/debug/wamn" build apps/edge_device
WAMN_FLOW_HTTP_COMPONENT="$CARGO_TARGET_DIR/wasm32-wasip2/release/http_route.wasm" \
  cargo test --locked --offline -p wamn-control --test edge_bundle -- --include-ignored
WAMN_FLOW_HTTP_COMPONENT="$CARGO_TARGET_DIR/wasm32-wasip2/release/http_route.wasm" \
  cargo test --locked --offline -p wamn-control --test release_one_digest -- --include-ignored
```

The generated clients of the host routes run against the same HTTP shell.
`control_client_live` serves each set and runs `apps/platform_ui/test/control-client.live.test.ts` and `control-org-client.live.test.ts` with `pnpm`, so run `pnpm install` at the repository root first:

```bash
WAMN_FLOW_HTTP_COMPONENT="$CARGO_TARGET_DIR/wasm32-wasip2/release/http_route.wasm" \
  cargo test --locked --offline -p wamn-integration-tests --lib control_client_live -- --ignored
```

The edge route, device and forward tests run in the workspace run.
Each test binary builds the edge release bundle of [`apps/edge_device`](../../apps/edge_device/README.md) once, when its first test starts.
It reads the component from the build output of `wamn build apps/edge_device`, so run that build first. Cargo builds the `http-route` guest as the Dockerfile does. The guest must match its pin.
The bundle writer composes the guest with the component into the entry that the edge serves.
The dev loop's bundle writer admits the component and writes the bundle, with no session.
The tests sign their own sessions.
The device test writes scale frames into a pseudo-terminal pair, which is its virtual serial port.
The forward test runs the `wamn-edge` binary as a child, kills it with SIGKILL twice, and serves an HTTPS platform with its own certificate authority.

```bash
cargo test --locked --offline -p wamn-edge --test route --test device --test forward
```

The test `samples_reach_a_live_platform_once_across_kills` forwards to a real platform: the `sample.record` route of [`apps/edge_samples`](../../apps/edge_samples/README.md) on a [developer session](development-loop.md).
Start the session with `--package` and `--overlay-root` set to `apps/edge_samples`.
The host serves plain HTTP, and the forward accepts only HTTPS, so put a TLS proxy in front of the served route address.
The proxy must set the `Host` header to the route host that the session prints.
Write the token of `operator-pat.json` in the environment directory to a file with mode 0600.
Then run the test with the proxy address and the certificate authority of the proxy:

```bash
WAMN_EDGE_LIVE_URL="https://127.0.0.1:<proxy port>/sample/record" \
WAMN_EDGE_LIVE_TOKEN_FILE=<token file> WAMN_EDGE_LIVE_CA_FILE=<proxy CA file> \
  cargo test --locked --offline -p wamn-edge --test forward \
  samples_reach_a_live_platform_once_across_kills -- --ignored --exact --nocapture
```

The edge runs the `sample.read` command of the `edge_device` bundle on each frame, and forwards the sample.
The test sends each of its two samples twice, and prints the tag of its frames.
Count the rows of that tag in `edge_samples.sample` of the session's target database: the count must be 2.

Run the local SQLx transaction case with its component:

```bash
cargo build --manifest-path apps/Cargo.toml --locked --offline \
  --target wasm32-wasip2 -p sqlx-command
WAMN_SQLX_TRANSACTION_COMPONENT="$CARGO_TARGET_DIR/wasm32-wasip2/debug/sqlx_command.wasm" \
  cargo test --locked --offline -p wamn-runtime --test sqlx_transaction_live \
    sqlx_command_commits_rolls_back_and_obeys_current_user_rls -- --ignored --exact
```

## Prekind render and native host tests

These cases use the environment release chart without a cluster.
The render cases need Helm 4 on `PATH`.
The native case also needs PostgreSQL 18 binaries, a local `nats-server` executable, and the guest toolchain from [building](building.md).
Use a prebuilt `nats-server`, as the other native cases do. Do not compile it.
Build every input in this checkout's own target directory. Replace `<checkout>` with a name for the checkout:

```bash
export CARGO_TARGET_DIR="$HOME/.cache/wamn-target/<checkout>"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='kache test-runner'
cargo build --locked --offline -p wamn-ctl --bin wamn
cargo build --locked --offline -p wamn-host -p wamn-test-infrastructure --bins
RUSTFLAGS="$(tools/guest-rustflags)" cargo build --locked --offline --release \
  --manifest-path apps/Cargo.toml --target wasm32-wasip2 -p http-route
RUSTFLAGS="$(tools/guest-rustflags)" cargo build --locked --offline \
  --manifest-path apps/Cargo.toml --target wasm32-wasip2 -p materializer
for guest in transform http-request http-page; do
  RUSTFLAGS="$(tools/guest-rustflags)" cargo build --locked --offline \
    --manifest-path apps/platform/no-std/Cargo.toml --target wasm32-wasip2 -p "$guest" || exit "$?"
done
"$CARGO_TARGET_DIR/debug/wamn-test-postgres" --database prekind_build --url-env DATABASE_URL -- \
  "$CARGO_TARGET_DIR/debug/wamn" build apps/edge_device
```

The HTTP route guest needs release mode because composition requires its committed SHA-256 digest.
The package builder fixes the device guest to release mode and records its virtualized bytes in the build receipt.
It writes the device component and receipt under `apps/target/wamn/edge_device` in this checkout.
The native programs, materializer, and three HTTP workflow guests use debug mode.
The `wamn-control` library tests need `apps/target/wamn/platform_fixture_overlay/wamn.json` from the initial checkout preparation.
If that file is absent, use the database runner above to build `apps/platform_fixture` and `apps/platform_fixture_overlay`, in that order.

Set the paths to these built inputs, then run the native case:

```bash
export WAMN_HOST_BINARY="$CARGO_TARGET_DIR/debug/wamn-host"
export WAMN_NATIVE_C_NATS_BIN="$(command -v nats-server)"
export WAMN_FLOW_HTTP_COMPONENT="$CARGO_TARGET_DIR/wasm32-wasip2/release/http_route.wasm"
export WAMN_MATERIALIZER_COMPONENT="$CARGO_TARGET_DIR/wasm32-wasip2/debug/materializer.wasm"
cargo test --locked --offline -p wamn-control --test environment_apply_kind \
  prekind_host::prekind_http_host_runs_and_rejects_respond_without_caller \
  -- --ignored --exact --nocapture --test-threads=1
```

The case resolves rendered Secrets through the production credential provisioner and starts private PostgreSQL and NATS servers.
It starts the debug host with rendered arguments and local component files.
It requires readiness, running HTTP and materializer workloads, and a completed manual HTTP workflow with the expected response.
Its old `respond` terminal must fail with `respond-without-caller` after the successful workflow.
That failure proves that rendering alone cannot establish workflow success.
The case prints its elapsed time and removes its temporary databases, processes, and component files.

Run the phase 1 cases by their full names:

```bash
for name in prekind_environment_renders prekind_rejects_old_missing_conn_reader \
  prekind_rejects_old_rollback_without_events prekind_receiving_pat_renders prekind_wms_pat_renders; do
  cargo test --locked --offline -p wamn-control --test environment_apply_kind \
    "prekind_render::$name" -- --exact --nocapture --test-threads=1 || exit "$?"
done
```

## Test-database isolation

Tests never connect to or change an interactive development database.
A database test takes its PostgreSQL from the `wamn-test-postgres` crate in `test-support/postgres`, and it reads no database variable.
The crate requires the PostgreSQL 18 binaries under `/usr/lib/postgresql/18/bin`.

`wamn_test_postgres::database()` creates a database with a unique name that the calling test owns.
The first call starts one private server for the test process.
That server stops, and its directory is removed, when the test process exits, also after a panic.
Dropping the returned value drops the database.
A cleanup error fails the test. During an existing panic, cleanup reports the error without starting a second panic.
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
| tenant+platform | `wamn_project_state::test_database::tenant_platform()` | the tenant floor and `deploy/sql/app-schema.sql` |

A test of provisioning, migrations, or grants executes that operation and does not start from a floor that already performed it.
Exercise the intended role because a superuser bypasses row security.

An [ignored test](#ignored-tests) takes its database from the same functions.

For Docker fixtures, choose a new container name and an unused loopback port.
Make sure that a real query succeeds through the same connection path that the command uses.

The `wamn-test-postgres` runner binary in `wamn-test-infrastructure` serves delivery tooling and the manual generation commands below.
Release qualification uses it for the server that `wamn build` uses.
It starts a server, creates the named database, and optionally applies migrations and history tables.
It removes inherited PostgreSQL variables, sets each `--url-env` variable to the database URL, and runs the command.
It stops the command's process group and removes its server on completion, failure, or handled interruption.

## Ignored tests

An ignored test needs built components, Docker, a cluster, a broker, or a registry.
Its ignore reason starts with `requires:` and lists the prerequisites, as in `#[ignore = "requires: <name>, <name>"]`.
A name in upper case is an environment variable that must be set and not empty.
Any other name is a program that must be on `PATH`.
The first statement of the test calls `wamn_test_postgres::require_prerequisites` with the names of the upper case and program entries.
When a prerequisite is missing, the test fails before it does any work, and the message names every missing prerequisite.
Some inputs do not fit this form, such as a binary beside the test executable, one of two inputs, or a built component.
The reason describes such an input in words, and the test checks it directly.
An ignored test does not return early or skip.

To run the ignored tests of the selected packages, use `tools/test-changes --cluster`.
They run one at a time.
To select tests by name, add `--name FILTER`, as [select the relevant tests](#select-the-relevant-tests) describes.

Run the generated exclusion test of the platform fixture through its PostgreSQL fixture:

```bash
cargo run --locked --offline -p wamn-test-infrastructure --example exclusion_diagnostics
```

The fixture starts disposable PostgreSQL, applies the platform fixture migration, and adds a test-only exclusion constraint on `widget`.
It captures actual update failures under a non-superuser role.
It regenerates the platform fixture code in a temporary copy.
Then it runs the ignored test `error::tests::generated_update_exclusion_from_postgres` of `wamn-platform-fixture-data-access`.
`WAMN_EXCLUSION_DIAGNOSTICS` supplies the server diagnostics to that test.
The fixture removes its database, generated copy, diagnostics, and build output when it finishes.
The application schemas and generated files in the checkout do not change.

Run the generated exclusion test of Receiving through its own PostgreSQL fixture:

```bash
cargo run --locked --offline -p wamn-receiving-tests --example exclusion_diagnostics
```

The Receiving fixture does the same steps with the Receiving migration and a test-only exclusion constraint on `purchase_order`.
Then it runs the ignored test `error::tests::generated_update_exclusion_from_postgres` of `wamn-receiving-data-access`.

## Capture a run

Ordinary tests need no result directory.
Report the command, source revision, and pass, fail, or skip outcome.
Some cluster tests and tools write diagnostic files.
Run output stays local and does not go into the checkout.
For a command that needs a new result directory, create a temporary one:

```bash
WAMN_RESULTS="$(mktemp -d)"
```

Keep source unchanged while a command runs.
Use diagnostic output to understand failures and the actual execution boundary.
Keep credentials in private files and out of shared output.
No permanent archive or result-only commit is required, except the sweep log of an epic closeout ([the full sweep](#the-full-sweep)).
[Result interpretation](../testing/evidence.md) defines passes, failures, and unexecuted cases.

## The full sweep

Workspace sweeps require an explicit user request.
Do not restart a stopped sweep or start broad runs at cleanup boundaries.
If the user requests a workspace sweep, use the retained command:

```bash
cargo test --workspace --locked --offline --features wamn-ctl/ops \
  --no-fail-fast -- --nocapture --test-threads=1 \
  > "$WAMN_RESULTS/workspace.log" 2>&1
```

Record its exit status before running another command.
Do not pipe the command through `tail` or another output filter.
`--workspace` selects all root members, including members outside Cargo's defaults.
`--features wamn-ctl/ops` builds the targets that require the `ops` feature.
The command does not turn on the `cluster` feature of `wamn-receiving-tests` and `wamn-wms-tests`, so it compiles no kind cluster test and starts no cluster.
`--no-fail-fast` retains later binary results after a failure.

The sweep runs no [ignored test](#ignored-tests), because each one needs inputs that a general test run does not have.
The ignored tests run in [the live stage](#the-live-stage).
`--nocapture` shows the output of each test.

A cluster stage runs alone, and only when the epic changed a file under the cluster tests.
Before a cluster stage starts, tell the other sessions on the machine.
Two cluster runs at the same time overload the machine, and both fail.
The cluster stage command is:

```bash
cargo test --locked --offline -p wamn-receiving-tests -p wamn-wms-tests \
  --features wamn-receiving-tests/cluster,wamn-wms-tests/cluster --lib cluster:: \
  -- --include-ignored --nocapture --test-threads=1 \
  > "$WAMN_RESULTS/cluster.log" 2>&1
```

The filter `cluster::` selects the cluster journeys and the plain tests of the cluster modules, which also compile only with the feature.
[Cluster tests](cluster-tests.md) describes the cases.

Record the start and end time of each run, and time its build apart from its tests.

Keep one sweep log per epic closeout in the repository, at `tests/sweeps/<epic>-<commit>.log`.
`<epic>` is the Beads id of the epic, and `<commit>` is the short hash of the commit that the sweep ran on.
Set `WAMN_RESULTS` outside the checkout, so that the run writes nothing into the tree.
After the sweep ends, copy `$WAMN_RESULTS/workspace.log` to that path.
Commit the log with the closeout.
If the sweep has failures, run the same command once on the base commit of the branch.
Then list the failures that differ by name.
Do not compare with the log of an earlier closeout, because the commits of other chains between them add failures that the epic did not cause.

If the user requests the separate contract command, run it after the sweep, including when the sweep fails:

```bash
tools/contract-diff run > "$WAMN_RESULTS/contract-diff.log" 2>&1
```

It runs the authoring contract and guest `http-route` adversarial tests.
The guest test belongs to another workspace, so the root sweep cannot reach it.
`tools/contract-diff dry-run` only prints the commands.

Report failures, ignored cases, explicit skips, filtered cases, and executed passes separately.
Report each failure's package, target, full test name, and actual cause.
A changed test name does not establish a new failure or remove an earlier failure.

## The live stage

The live stage runs named ignored tests with their inputs set, apart from the full sweep.
A cluster day or a live check cites the live stage on its bead.
Run each named test with its exact filter:

```bash
cargo test --locked --offline -p <package> --test <target> <test> \
  -- --ignored --exact --nocapture --test-threads=1 \
  > "$WAMN_RESULTS/live.log" 2>&1
```

For a test in a library target, use `--lib` in place of `--test <target>`.
Set each input that the ignore reason of the test names before the run.
The bead names each test, each input by its variable name, and each result.
A credential, such as `RESEND_API_KEY`, stays in the environment of the stage.
Never write its value into the bead or the log.

## Live prerequisites and troubleshooting

An exact filter can select zero tests and still return success.
For a named case, require its exact result row and one executed case.
An ignored test needs `--ignored` or `--include-ignored` after Cargo's `--` separator.
Do not infer execution from the aggregate Cargo pass count.

| Existing reference | Current owner and required setup |
| --- | --- |
| `[RUN-PLANE-RECONCILE]` | `crates/control/lib/tests/run_plane_live.rs`: runs by default on the test server and holds the process lock |
| `[CLAIMS-LIVE]` | Runtime `plugins::wamn_postgres::claims::tests` `live_*` cases: run by default on the test server, and the cases that create fixed roles hold the process lock |
| `[EFFECT-SNAPSHOT]` | `crates/platform/runtime/tests/effect_snapshot_live.rs`: runs the production cluster snapshot as a provisioned HTTP admitter on the test server and holds the process lock. |
| `[R18-NEG]` | Runtime `plugins::wamn_postgres::claims::tests::live_scs_off_server_fails_checkout_closed`: runs by default on a separate server started with `standard_conforming_strings=off` |
| `[EVT-READER]` | `services/cdc-reader/tests/event_reader_live.rs`: ignored and selected by `tools/test-changes --cluster`, starts its own server with `wal_level=logical`, and needs `WAMN_READER_NATS_URL` |
| `[CAPTURE-GAP]` | `crates/control/lib/tests/capture_gap_live.rs`: ignored, starts its own server with `wal_level=logical`, and needs `WAMN_READER_NATS_URL`. It does not test the refusal of an active slot, because only a streaming reader holds a slot active |
| `[SQLX-TRANSACTION]` | `crates/platform/runtime/tests/sqlx_transaction_live.rs`: ignored and selected by `tools/test-changes --cluster`, takes its database from the test server, and needs `WAMN_SQLX_TRANSACTION_COMPONENT` |
| `[MGMT-LIVE]` | `services/scenario-worker/tests/management_live.rs`: runs by default on the test server and holds the process lock |
| `[STD-GUEST-VIRTUALIZATION]` | `tests/integration/src/virtualized_std_guest.rs`: ignored and selected by `tools/test-changes --cluster`, and needs built guest files and its explicit `WAMN_STD_VIRTUALIZATION_*` inputs |
| `[EVT-C-CDC]` | `tests/integration/src/cdcbench.rs`: runs through `wamn-gates cdcbench`, starts its own server with `wal_level=logical`, and needs a JetStream NATS at `--nats-url` |

The source owners declare additional credentials, artifact paths, and selected assertions.
Do not substitute shared services for missing test inputs.
For deployed runs, use [cluster commands](cluster-tests.md).

`production_operator_authentication_and_operation_authorization` keeps `identity.log` and `test.log` in a new `wamn-route-auth-*` directory under `${TMPDIR:-/var/tmp}`.
A failure prints both paths; the Receiving qualification cases keep these logs in their existing results directory and print their paths on failure.

### Record history retention test

`services/ctl/tests/prune_record_history_live.rs` tests the record history retention verb.
It creates roles, so each case holds the process lock and starts from the tenant+platform floor.
The `ops` feature builds `wamn-ctl-ops` beside the test binary:

```bash
cargo test --locked --offline -p wamn-ctl --features ops \
  --test prune_record_history_live -- --nocapture
```

Each case applies a fixture package through `wamn-ctl apply-package` and runs `wamn-ctl-ops prune-record-history`.
`prune_removes_only_the_expired_prefix_of_each_row` also mints the run retention generation to test the refusals.
`a_retention_change_waits_for_a_running_prune` holds the audit retention lock while the verb and a retention change wait on it.

### Run history retention test

`services/ctl/tests/prune_run_history_live.rs` tests the run history retention verb.
It creates roles, so it holds the process lock and starts from the tenant floor.
It needs the `ops` feature:

```bash
cargo test --locked --offline -p wamn-ctl --features ops \
  --test prune_run_history_live -- --nocapture
```

The case applies `run-state.sql` and `run-queue.sql`, mints the run retention generation, and runs `wamn-ctl-ops prune-run-history`.
It tests the removed and kept runs, the refusals, and the grants of the generation.

### Development identity lifecycle

The development command case tests application rebuilds and owned process cleanup.
Build the test identity binary with `cargo build --locked --offline -p wamn-identity --features test-util`.
The `test-util` feature permits only a loopback mail endpoint with a dummy API key. Production builds do not include this path.
The focused identity case also covers application database recreation:

```bash
RESEND_API_KEY=unused-development-fixture RESEND_FROM='WAMN <fixture@example.invalid>' \
  cargo test --locked --offline -p wamn-ctl --test pat_bootstrap_live \
  development_identity_survives_target_recreation_and_owned_teardown -- --ignored --exact
```

On a disposable stack, the `pat_for_disposable_stack` example of `wamn-test-infrastructure` issues one management-author PAT into a mode 0600 file. It starts and stops this identity process, and its header gives the usage.

### Local saved-edit acceptance

Use a clean linked worktree reserved for this test.
Set `SOURCE` to its absolute path.
Export `CARGO_TARGET_DIR` with the path to its separate absolute build directory.
The case changes authored files, observes the running application, and restores the saved source.
Run this case alone.
The fixture requires PostgreSQL 18 binaries, Docker, and the existing development service images.
The case also requires SQLx CLI 0.9.0 as `cargo-sqlx` and `jq` on `PATH`.

Build the native programs, the packages, the HTTP and materializer guests, and the exact test binary from that worktree, with a PostgreSQL server URL in `DATABASE_URL`:

```bash
cd "$SOURCE"
cargo build --locked --offline \
  -p wamn-ctl -p wamn-host -p wamn-identity -p wamn-cdc-reader \
  -p wamn-test-infrastructure -p wamn-receiving-tests --bins --example delivery_timings
cargo build --locked --offline --manifest-path apps/Cargo.toml \
  --target wasm32-wasip2 -p http-route
RUSTFLAGS="$(tools/guest-rustflags)" cargo build --locked --offline --release \
  --manifest-path apps/Cargo.toml --target wasm32-wasip2 -p materializer
"$CARGO_TARGET_DIR/debug/wamn" build apps/wamn_receiving apps/client_acme_receiving
cargo test --locked --offline -p wamn-receiving-tests --lib --no-run
```

Set `WAMN_DEV_ENV_MATERIALIZER_COMPONENT` to the absolute path of that `materializer.wasm`, `$CARGO_TARGET_DIR/wasm32-wasip2/release/materializer.wasm`.

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
If you put `--wms` before `--`, the fixture also starts MinIO and makes the `labels` bucket.
It then sets `WAMN_DEV_ENV_CREDENTIALS_FILE` to a mode 0600 file with the MinIO credential under the handle `labels-store`.
It sets `WAMN_RECEIVING_DEV_MINIO_ENDPOINT` to the MinIO address.
The owned event broker admits the materializer consumers of the packages that the command gives `wamn dev up`.
These are `wamn_receiving` and `client_acme_receiving`, or `wamn_wms` with `--wms`.
The fixture sets the CDC reader binary and the event publisher credential for `wamn dev up`.
It also sets the event materializer credential, which the broker scopes to the source stream.
It sets `WAMN_DEV_ENV_ROOT` to `delivery-timings/env` in the target directory, which survives runs, so the compile cache survives too.
Before the command starts, the fixture counts the files under `dev-wasmtime-cache/modules` in that root, without the `.stats` files.
It writes the count to `cache-state.json` in the results directory, so that you can read a cold run as cold.
`wamn-test-postgres` starts its server with `wal_level=logical`.
It sets every variable that the case declares, except the two that the command sets.
The case starts its own PostgreSQL server, because the development environment resets the control store of the whole server.
The case requires authenticated application results after code, SQL, and schema edits.
It also requires retained data for compatible edits and for an appended migration.
It requires refusal of invalid SQL, and a new database after a declared definition owner change.
Require one executed passing case, successful resource cleanup, and restored source before reporting success.
This correctness case does not report performance measurements.

### Bounded HTTP reuse

Use the exact debug integration test binary and debug `http_request.wasm` from the intended build.
Keep source unchanged between that build and both runs.
The runner requires the local `registry:2` image.
Each test starts its own PostgreSQL server from the local PostgreSQL 18 binaries.
Replace `<hash>` below with the exact suffix from the integration test build.

```bash
export WAMN_HTTP_REUSE_TEST_BINARY="$CARGO_TARGET_DIR/debug/deps/wamn_integration_tests-<hash>"
export WAMN_HTTP_REUSE_COMPONENT_WASM="$CARGO_TARGET_DIR/wasm32-wasip2/debug/http_request.wasm"
bash tools/http-reuse-run trusted_http_route::tests::real_http_guest_reuses_connections_without_reusing_authority
```

Each command creates a fresh registry container, runs its exact test, and removes only owned containers and volumes.
These cases test connection reuse and authority isolation without measuring throughput.

### Platform and environment apply on kind

Both cases record failures from independent checks and continue.
If a later check needs a failed setup step, the case stops.
The final error names every recorded failure, including any setup error.
Both cases remove their cluster and registry containers after either outcome.

The platform test needs Docker with buildx manifest reads, kind, Helm, kubectl, pnpm, OpenSSL, and the gcloud credential helper on PATH.
It needs network access to the GCP registry and public registries.
It also needs the [package build tools and workspace dependencies](building.md#wamn-build), but no image build tools.
It reads the debug binaries `wamn` and `wamn-ctl` from `$CARGO_TARGET_DIR/debug`.
The local platform UI bundle must exist before the registry stamp.
It needs no existing database, bucket or DNS.
It writes files under `/var/tmp` and deletes only its own cluster and registry containers after success or failure.
The image registry uses HTTP on the kind network.
A second registry serves package and release artifacts over TLS with a fixture certificate.
The test passes that registry and its CA file directly to the installer.
It uses Receiving and WMS as package fixtures through step 4.14.
Receiving publishes a release without a wiring.
WMS publishes `inventory_move_and_label` through one Job pod with a native gate sidecar.
The main container mints its PAT in memory, gates the wiring, authors it, and publishes its release.
The test requires both release lines, the gate startup log, and deletion of the Job-owned database login Secrets.
The unchanged rerun must return the same WMS digest and leave its saved Job log unchanged.
The gate does not execute the WMS blobstore effect, so this test needs no external bucket.
The fixture copies `deploy/gcp/host-group-receiving.yaml` into its own cluster directory.
The test records both printed digests in its own environment files.
WMS stays unqualified and runs first in the refusal assertions.
The unchanged rerun must skip every step through 4.14 and refuse that same WMS release.
The test then calls `delivery::qualification::qualify` on the host for Receiving, which has one required case.
It supplies the fixture release registry, its credential file, its CA path, and the stamped chart.
It prints one line with the qualification wall time and result.
Qualification starts its own case cluster through the host Docker socket.

The fixture then copies the exact role manifests and blobs to its TLS registry and stamps the same image digests at reachable addresses.
It retains the original chart in `qualification-chart` and requires an unchanged qualification key.
Containerd uses the existing HTTP mirror for native images, while workload processes read roles over TLS.
The Receiving host group mounts the fixture registry credential and CA Secret.
These changes affect only temporary fixture files.

A separate environment directory selects qualified Receiving for the remaining install and its unchanged rerun.
Step 4.17 must serve that release.
Step 4.18 provisions capture from the schemas of the environment's application packages.
Capture does not depend on an event workflow.

`environment_apply_kind` reuses `rest_connector`, `rest_consumer_fixture`, and `wamn_integration`.
It requires one publication and one logical replication slot for the environment.
The publication and `registry.event_readers` must name both `rest_consumer_fixture` and `wamn_integration` schemas.
Every captured entity table must use full replica identity (`relreplident = 'f'`).

The test authors and enables an event workflow for `item` and another for `setting`.
The native CDC reader captures one row update in each schema.
Each delivery ID must identify exactly one run of its own trigger in `wamn_run.runs`.
Neither trigger can receive a run from the other schema's change.
The test waits at most 150 seconds for completed runs and broker acknowledgments.

Step 4.19 must report `skipped: no bucket` for the upload and its check, with no web Job or completion record.
Step 4.19.1 must roll out the control host and report `skipped: no bucket` for the platform UI upload and its check.
The fixture selects identity's private local mail file in a memory volume, so the invite sends no external email.
The owner invite Job must run once with `--org-admin`.
The test requires one invitation and no invitation link or secret in the logs.
The rerun must skip the invite and leave its saved Job log and mail file unchanged.
It must preserve the control Secrets and remove the temporary Job-owned Secrets.
The final rerun must skip every implemented step through 4.21 and report 4.15 and 4.16 as owner steps.
Step 4.20 must install `edge/wamn-edge-web` with the stamped image and wait for its Deployment to become available.
Its service account can get only the named release ConfigMaps in `hosts`.
The empty bucket must start no bucket Job.
The nginx Service must expose HTTPS without a GCP NEG annotation on kind.
Its Deployment must mount `wamn-edge-tls` and the identity CA.

Step 4.21 uses the existing pinned curl image in Jobs that reach nginx through its cluster Service.
These Jobs add a public image pull to the fixture requirements.
They preserve the route hostname and trust the public certificate from the self-signed Secret.
The stored result must contain one API status of 401 and a matching certificate serial for each host.
It must record skips for the public load balancer and absent web bucket.
The test reads these objects after the phase and makes no new assertion on progress or log text.
The unchanged rerun must preserve the proxy Deployment and HTTPS completion record, including their resource versions.
It must leave no HTTPS Job running.
Both phases retain the PAT, database credential, and unchanged Secret assertions.
The GCP greenfield covers uploads to a real bucket.

Use the [local train build](building.md#local-train-builds) to prepare the images and stamped chart.
Keep its `WAMN_RELEASE_CHART` and empty `DOCKER_CONFIG` exports for both cases.
No GCP credentials, `gcloud`, or GCP cluster document is needed on this path.
The chart version and all seven images must match this checkout.
The build also prepares the UI bundle that the fixture reads.

Build the debug test binaries before the cases:

```bash
cargo build --locked -p wamn-ctl --bins -p wamn-cdc-reader
```

When the owner requests kind tests, run both cases together from the prepared shell:

```bash
cargo test --locked -p wamn-control --test platform_apply_kind -- --ignored --nocapture &
platform_pid=$!
cargo test --locked -p wamn-control --test environment_apply_kind -- --ignored --nocapture &
environment_pid=$!
platform_status=0
wait "$platform_pid" || platform_status=$?
environment_status=0
wait "$environment_pid" || environment_status=$?
test "$platform_status" -eq 0 && test "$environment_status" -eq 0
```

The environment case also needs the package and OAuth1 fixture inputs described below and in its test source.
Each case creates its own cluster.
Without `WAMN_RELEASE_CHART`, the older fallback reads `deploy/gcp/cluster.k` and requires the agent GCP credentials.
Always supply the chart for local train tests.

The platform fixture starts a `registry:2` container named `<cluster_name>-registry` on the `kind` Docker network.
Docker selects a free loopback port, and `cluster.k` names the registry as `localhost:<port>`.
The fixture copies five images and two role components from the train registry, then stamps their local digests.
The environment JSON gets its gates image from this local stamp.
Step 4.6 builds the Receiving package with a temporary PostgreSQL 18 container and stamps the local registry images.
The test never builds platform images.

The fixture creates `identity/identity-resend` with a stand-in key before the install.
The test runs `platform apply` through section 4.14.
The runner image must contain the pinned KCL compiler for the first environment apply.
It requires ready PostgreSQL and NATS.
An anonymous CONNECT must return `Authorization Violation` without `PONG`.
It also requires both `identity-ca` ConfigMaps, identity database and session target Secrets, and the completion ConfigMaps for the environment and identity Jobs.
The identity Job requests public keys over TLS and requires the active key in the response.
The test refuses credentials in progress or Job logs.
After the qualification refusal, an unchanged apply must skip every step through 4.14.
The broker and session target Secrets must keep their resource versions.

The separate environment test needs Docker, kind, Helm 4, kubectl, OpenSSL, and the release chart that `build-platform` stamps.
Build `apps/edge_device` with `wamn build` first.
Its [test source](../../crates/control/lib/tests/environment_apply_kind.rs) describes the required images and fixtures.

```bash
WAMN_RELEASE_CHART=<chart> cargo test -p wamn-control --test environment_apply_kind -- --ignored --nocapture
```

### Registry image archive in kind

This test runs `tools/journey-image-cache` and `case_release::image_archive` of `wamn-gate-harness` against a private registry with a login and TLS.
It needs the local `registry:2`, `httpd:2-alpine` and kind node images.
Tell the other sessions on the machine before the run, because it creates a kind cluster.

```bash
cargo test --locked --offline -p wamn-conformance-tests --test registry_image_archive \
  -- --ignored --exact kind_nodes_import_the_pinned_digest_and_hold_no_credential --nocapture
```

The test pushes an image and a guest by digest, and two kind nodes import the image archive.
It makes sure that CRI reports the tag and only the pinned digest, and that no node holds the login.
It removes its cluster, registry container, images and private directory.

## Application generation and SQLx

Follow [test-database isolation](#test-database-isolation) before these database-backed commands.

`wamn build` generates every application ([building](building.md#wamn-build)).
Its output lies in `apps/target/wamn/<package>`, and Git holds none of it.
Build every package before you compile the apps workspace, the application test crates, or an application web:

```bash
DATABASE_URL=postgres://postgres:<password>@127.0.0.1:<port>/postgres \
  cargo run --locked --offline -p wamn-ctl --bin wamn -- build \
  apps/platform_fixture apps/platform_fixture_overlay apps/wamn_receiving \
  apps/client_acme_receiving apps/wamn_wms apps/edge_samples apps/edge_device
```

The platform fixture is an application too, and it builds the same way.
The platform owns it, and a platform test takes it instead of an application.
The crate `wamn-fixture-package` states its paths, so a test reads them without a literal of its own.
Do not edit generated Rust directly.

The development loop prepares SQLx metadata for every package with SQL declarations in its Generate stage.
The platform verifier discovers queries and Rust types from generated source maps, and compiles the exact SQL through SQLx macros.
It writes the metadata to `.sqlx` in the package's build output.
It prepares an application only when one of these inputs changed:

- the input digest in the package's `build.json`, which covers the package files, the apps workspace `Cargo.lock`, and the generator identity
- the digest of each base package in the same `build.json`, which for Acme changes with a Receiving change

The loop compares these inputs with the inputs of its last successful preparation in the session.
A new session prepares once. A failed or interrupted preparation runs again in the next cycle.
A Rust-only change does not prepare.

The loop calls `wamn_schema_generator::prepare_sqlx_metadata` in its own process, with the URL of the target database.
The verifier requires SQLx CLI 0.9.0.
No separate command prepares the metadata by hand.

### wamn build

The live build tests use the PostgreSQL 18 binaries and start their own server, as the other generator database tests do:

```bash
cargo test --locked --offline -p wamn-schema-generator --test build_live
```

They copy the platform fixture and build it into their own output root.
A build with an old manifest and obsolete client output must match a build into an empty target.
Both builds must emit the authored operation bindings and components, with identical bytes and `build.json`.
The tests also check that a left-out statement type is derived and that the nullability rule of [building](building.md#wamn-build) holds.
One test composes Acme over Receiving into a copy of their build output and checks that the composed component embeds the exact bytes of the Receiving component. Run `wamn build apps/wamn_receiving apps/client_acme_receiving` first.

### Generated TypeScript bindings

The generated TypeScript is type-checked by hand, not by a test and not by a build.
Nothing in `cargo build` needs Node, and the ignored `control_client_live` tests run it.
To type-check the bindings for the platform fixture, run:

```bash
cargo run --locked --offline -p wamn-schema-generator --example check_client_ts
```

The command emits the fixture bindings into a temporary directory, writes a strict `tsconfig.json` beside them, and runs `tsc --project` on that directory.
Strict here means `strict`, `noImplicitAny`, `strictNullChecks`, `exactOptionalPropertyTypes`, `noUncheckedIndexedAccess`, and `noUnusedLocals`.
Pass a directory as the one argument to keep the emitted source for reading.
The command reads `tsc` from `PATH`. If `tsc` is absent, the command refuses and names it.
It needs TypeScript 5.5 or later.
The emitted modules import the hand-written runtime by its package name, and the command maps that name to `web/runtime` in this checkout.

The clients of the host routes are build output, and Git does not hold them.
The generator writes the client of the application host routes, `@wamn/control-client`, to `apps/target/wamn/wamn_control/client-ts`.
It writes the client of the control routes, `@wamn/control-org-client`, to `apps/target/wamn/wamn_control/control/client-ts`.
It makes them from the contracts in `crates/catalog/model/src/host_route/contracts` and `host_route/control/contracts`.
Every web script that reads the clients writes them first through `web/host-route-clients.mjs`, so you do not need to write them yourself.
To write them without a web script, run:

```bash
cargo run --locked --offline -p wamn-schema-generator --example materialize_host_route_client
```

`pnpm run check` in `apps/platform_ui` type-checks them, because that package maps `@wamn/control-client` and `@wamn/control-org-client` to them.
The repository policy lint refuses a tracked file in a `client-ts` directory outside `apps/`.

### Web packages

The web packages form one pnpm workspace, which `pnpm-workspace.yaml` at the repository root lists.
It includes `web/runtime`, `web/shell`, the wfe packages, `apps/platform_ui`, and `apps/wamn_receiving/web`.
Use pnpm for every command of these packages, not npm.
Install all of them once, at the repository root:

```bash
pnpm install
```

The workspace has one lock file, `pnpm-lock.yaml`, at the repository root.
The scripts of the application web packages write the host route clients with Cargo before they run.
So these scripts need the Rust toolchain, and the first run compiles the generator.

Prettier formats the hand-written web code, and ESLint lints it.
One configuration for each tool is at the repository root: `.prettierrc.json`, `.prettierignore` and `eslint.config.js`.
To make sure that the code is formatted and has no lint errors, run this at the repository root:

```bash
pnpm run check
```

The command fails on a lint error but not on a lint warning.
To format the code, run `pnpm run format` at the repository root.

### Web runtime

The runtime is hand-written TypeScript, and its own tests run under Node.
To type-check the runtime and its tests, run:

```bash
cd web/runtime && pnpm run check
```

To run its tests, run:

```bash
cd web/runtime && pnpm test
```

The tests read the case table at `web/runtime/test/classification-cases.json`.
If you change the table, run these tests.

### Platform UI

`apps/platform_ui` owns the shared Solid 2 shell and the platform screens.
After the workspace install, run these commands from the repository root:

```bash
pnpm --filter @wamn/platform-ui check
pnpm --filter @wamn/platform-ui test
pnpm --filter @wamn/platform-ui build
```

### Local platform sign-in

Local fixtures that run `wamn dev` must write an authored `environment.k` beside `dev.json`.
The examples in `tests/integration/fixtures/local-environments` declare application names separately from package IDs.
The loop refuses a missing file before it connects to services.

`local_control_projects_and_membership` runs the built platform UI in headless Chrome.
It starts `wamn dev up` on disposable services through the existing `delivery_timings` fixture.

The test uses platform verbs to enroll the org admin.
The admin opens the Users table controls and selects Invite user.
The invitation form returns to Users after submission and shows the delivery result.
The test reads the invitation code from the [local mail file](development-loop.md#wamn-dev-environment-developer-session) and passes it to the browser.
The second user sets a password on the platform UI invite page and signs in.
The admin deactivates and reactivates the user, then grants and revokes org-admin.
Each write must update the Users screen, including after a reload.

The fixture grants the admin membership in `receiving/dev`, requires both audiences, and chooses Control.
Control opens the single held project directly, or Projects when the viewer holds several projects. The shell root carries `data-ui`.
The fixture runs `wamn dev --config <root>/dev.json --overlay-root apps/wamn_receiving --hold` to install and start the Receiving release.
The browser opens the Projects table controls and selects Create project.
The form returns to Projects after submission.
The browser opens the new `browser-project` from the table.
The fixture then calls `provision_project_env` and `reconcile_journey_run_plane` to prepare `browser-project/dev` on the disposable PostgreSQL server.
It prepares the administration credential through `run_workload_action` and installs its emitted file in the local control host's credential directory.

The browser opens separate membership forms from the User and Project table toolbars.
Each form returns to its owning table after submission.
Record actions beside the title grant and revoke project-admin access.
Membership rows provide the Revoke membership action.
The User screen scopes every write to the principal in its address.
Each write must update the screen.
The second user opens its single project directly and can return to the organization. Addresses for `receiving`, Users, and User show no page.
The project-admin session uses the project names from `control.mine` and never calls `project.list`.
Project inactivation and activation must each show only the opposite action, including after a page reload.

The browser opens `receiving/dev` from the Environments table for the project.
It reads the runtime status and release profile, then toggles the state and reloads the page.
It follows the environment pane to Workloads, Live releases, Operations, and Roles, then reloads each table screen.
It expands workload conditions and the Operations reference tables, searches context choices, returns to a parent, and opens environment Members.
If identity limits session renewals, the browser waits for its `Retry-After` period before it reloads the same address.
The test prints its elapsed time and fails if it exceeds 15 minutes.

The browser uses a 1400 by 900 viewport and saves one PNG for each screen in `<results>/platform-ui-screenshots/`.
The Rust test and browser print that directory.
Identity files are `sign-in.png`, `invite.png`, `recover.png`, `reset.png`, `no-access.png`, and `audience-choice.png`.
Administration files are `control-layout.png`, `projects.png`, `project.png`, `environment.png`, `users.png`, and `user.png`.
Splash files are `environment-splash.png` and `control-splash.png`.
The browser compares their tile values with fixture status responses and requires one status read per environment.
Form files are `create-project.png`, `invite-user.png`, `project-grant-membership.png`, `member-grant-membership.png`, and `user-grant-membership.png`.
Access table files are `project-environments.png`, `project-members.png`, `member-memberships.png`, `user-projects.png`, and `user-memberships.png`.
Environment table files are `environment-workloads.png`, `environment-live-releases.png`, `environment-operations.png`, and `environment-roles.png`.
Expanded reference files are `workload-conditions.png` and `operation-references.png`.
Context files are `environment-switcher.png` and `environment-members.png`.
The other files are `receiving-layout.png` and `not-found.png`.
Recovery and reset captures show their entry forms. The reset capture includes the missing-code message.
The test retains these files after service cleanup.
For the WFE review, copy the PNG files from the printed directory to `/var/tmp/wamn-wfe-ui-report/` and attach them to the report.

Build the native programs and components described in [the development loop](development-loop.md).
Build the application bundles with `wamn build apps/wamn_receiving apps/client_acme_receiving`.
Build the platform UI with `pnpm --filter @wamn/platform-ui build`.
Build the fixture and test before the timed run:

```bash
TMPDIR=/var/tmp cargo build --offline -p wamn-receiving-tests --example delivery_timings
TMPDIR=/var/tmp cargo test --offline -p wamn-ctl --test platform_ui_live --no-run
```

The fixture needs Docker and its local service images, PostgreSQL 18 binaries, and Chrome.
It uses the Playwright installation in `web/shell`, so run `pnpm install` at the repository root first.
`CARGO_TARGET_DIR` must name the directory of the built native programs.
Set the two component variables to the corresponding built component files.
Run this command from the repository root:

```bash
export TMPDIR=/var/tmp
export WAMN_DEV_ENV_FLOW_HTTP_COMPONENT="$CARGO_TARGET_DIR/wasm32-wasip2/release/http_route.wasm"
export WAMN_DEV_ENV_MATERIALIZER_COMPONENT="$CARGO_TARGET_DIR/wasm32-wasip2/release/materializer.wasm"
results="$(mktemp -d /var/tmp/wamn-platform-ui-XXXXXX)/run"
"$CARGO_TARGET_DIR/debug/examples/delivery_timings" "$PWD" "$CARGO_TARGET_DIR" "$results" -- \
  cargo test --offline -p wamn-ctl --test platform_ui_live \
  local_control_projects_and_membership -- --ignored --exact --nocapture
```

The test uses a fixed fixture password and sends no email.
Its PostgreSQL server and the fixture's containers stop after the test.
`dev down` stops the owned web server, control host, and identity service.
The results directory keeps `platform-ui-env/control-web.log` for failure diagnosis.

### Local Receiving screens

`local_receiving_screens` drives the Receiving web app through real local services.
It uses the same prerequisites, component variables, and `delivery_timings` fixture as [Local platform sign-in](#local-platform-sign-in).
Build Receiving and the platform UI before the run.
The test seeds 26 purchase orders, one line per order, one supplier, one item, and one location in its disposable database.
It signs in, follows record links, pages the orders, changes a supplier, and records a receipt.
It also creates a supplier and uses the form cancel action.
The browser requires table-header actions, full-panel tables, literal uppercase breadcrumbs, and forms that return to their tables.

Build the test without starting services:

```bash
TMPDIR=/var/tmp cargo test --offline -p wamn-ctl --test receiving_ui_live --no-run
```

After you set the component variables from Local platform sign-in, run from the repository root:

```bash
export TMPDIR=/var/tmp
results="$(mktemp -d /var/tmp/wamn-receiving-ui-XXXXXX)/run"
"$CARGO_TARGET_DIR/debug/examples/delivery_timings" "$PWD" "$CARGO_TARGET_DIR" "$results" -- \
  cargo test --offline -p wamn-ctl --test receiving_ui_live \
  local_receiving_screens -- --ignored --exact --nocapture
```

The browser saves 1400 by 900 screenshots in `<results>/receiving-screenshots/`.
The files are `sign-in.png`, `audiences.png`, `purchase-orders.png`, `purchase-order.png`, `change-supplier.png`, `purchase-order-history.png`, and `receiving.png`.
It also saves `record-receipt.png`, `receipts.png`, `receipt.png`, `create-supplier.png`, `suppliers.png`, and `locations.png`.
The test prints the directory and retains it after service cleanup.
The service log stays at `<results>/receiving-ui-env/control-web.log`.

### Generated descriptions

`client_component.rs` emits typed descriptions for `@wfe/generator` from the pinned `web/wfe` package.
The generator integration test compiles fixture descriptions with TypeScript.
It covers table, form, repeated group, detail, and delete descriptions.
Application bindings own requests, validation, and value conversion.

Run `pnpm install --offline` at the repository root first.
The existing manual check uses the same test code:

```bash
TMPDIR=/var/tmp cargo run --offline -p wamn-schema-generator --example check_client_components
```

The command writes temporary modules and removes them after the check.
`check_client_ts` checks the unchanged TypeScript bindings separately.

### Shared Vite tooling and Receiving

`web/shell` keeps the shared Vite helper and the Playwright dependency for browser tests.
The platform UI owns the application shell and its tests.
To type-check the helper and the Receiving web application, run:

```bash
pnpm --dir web/shell check
pnpm --dir apps/wamn_receiving/web check
```

## Cleanup

The app and RC runners clean up their own named resources on success, failure, or handled interruption.
The RC runner removes its PostgreSQL and NATS containers with `docker rm --force --volumes`.
The package-build PostgreSQL fixture, both registries of `platform_apply_kind`, and the local package registry use `docker rm -f -v`.
These commands also remove anonymous volumes, which Docker creates for image-declared data directories.
Container restarts retain their data until cleanup removes the container.
Inspect their recorded cleanup result and remaining resource names before reporting completion.
For manual fixtures, remove only the exact container, cluster, image, volume, and private directory that your run created.

A killed cluster test (SIGKILL) does not run its cleanup, so its cluster and containers stay.
After a killed run, find the cluster that your run created with `kind get clusters`.
Its name starts with `wamn-receiving-` or `wamn-wms-`.
Remove it with the script of its application:

```bash
tools/receiving-cluster-journey-run remove <cluster>
tools/wms-cluster-journey-run remove <cluster>
```

The script deletes the kind cluster and its `<cluster>-postgres`, `-registry`, `-nats` and `-minio` containers.
Never remove a cluster that another session created.

The cluster journeys keep their built images between runs.
Each image is named for the source it contains, and a later run of the same source reuses it.
A run still removes its own per-run image tags at teardown.
It then keeps the three most recently used identities of each image repository and removes the rest.
An identity that another run holds is never removed.
The run records and leases live under `${XDG_CACHE_HOME:-$HOME/.cache}/wamn-journey-images`.

Use explicit cluster names, kubeconfig paths, and contexts for Kubernetes and Helm commands.
Never use `docker prune`, broad image removal, or name-substring cleanup.

Report unresolved cleanup failures and keep only output needed for current work or specifically requested by the user.
Before removing a worktree, preserve its source changes and branch.
Remove only that worktree and its owned target after no process uses them.

## Retained benchmark commands

The `wamn-gates` binary exposes these retained commands:

- `cdcbench` measures database change capture drain rate, lag, and logging cost.
- `walbench` measures PostgreSQL write-ahead log volume for individual operations and mixed workloads.
- `streambench` tests event storage, duplicate suppression, delivery order, and recovery.

Inspect their inputs without starting a run:

```bash
cargo run --locked --offline -p wamn-gates --bin wamn-gates -- cdcbench --help
cargo run --locked --offline -p wamn-gates --bin wamn-gates -- walbench --help
cargo run --locked --offline -p wamn-gates --bin wamn-gates -- streambench --help
```

The database benchmarks start disposable PostgreSQL through the existing test owner.
CDC also needs a built `wamn-cdc-reader` and an explicitly selected disposable NATS service.
`WAMN_CDC_READER_BIN` can select the reader executable.
Stream tests also need an explicitly selected disposable NATS service.
`streambench --mode all` refuses fewer than three replicas before it connects.
Do not use the frozen cluster as a fixture.

## Host and package runtime

Run the combined host's lifecycle tests:

```bash
cargo test --locked --offline -p wamn-host --bin wamn-host host::tests::
```

The shared queue tests run real admission, active-work cancellation, lease recovery, and authority cleanup.
They start a disposable PostgreSQL server.
The ignored cases of the module need more inputs and do not run here.

```bash
cargo test --locked --offline -p wamn-workflow --lib queue::automation_live:: -- --nocapture
```

For production authorization, component execution, and database behavior, run the [local application business tests](#local-application-business-tests).
Use the application delivery journeys for deployed startup, readiness, authenticated requests, queue delivery, and cleanup.

### Workflow schedules

The schedule task's unit tests run with paused time: the first tick after a restart, a row enabled or disabled between ticks, and ticks missed during a slow admission.

```bash
cargo test -p wamn-workflow --lib schedule::
```

The live test authors schedule workflows on a disposable PostgreSQL 18 database, admits due moments, and drains the runs through the real queue.
One case runs the host's schedule task in real time: a row enabled after the task starts fires at its next tick, and a disabled row fires at no later tick.
Another holds a lock on the rows across one minute: the late tick admits its own due moment, the missed tick is skipped, and the next tick fires.
Each of these two cases takes up to about three minutes.

```bash
cargo test -p wamn-control --test workflow_schedule_live -- --nocapture
```
