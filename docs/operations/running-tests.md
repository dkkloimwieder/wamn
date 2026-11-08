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
After the package build, `tools/test-changes` runs `wamn web build` for each selected application with `web/package.json`.
The standalone web artifacts supply browser files for the developer session tests.
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
- Any `Cargo.toml` or `Cargo.lock` also runs `tools/check-packages-alone`, which runs `cargo check -p` for each root workspace package on its own. A package that compiles only through a feature that another package turns on fails it.
- Canonical interfaces live under repository-root `wit/`. Router and runtime interfaces under `wit/` select all three workspaces because bindings read WIT outside Cargo dependencies. Changes to `wit/wamn-router-delivery-0.4/`, `wit/wamn-application-dispatch/`, and materializer WASI WIT select root and apps.
- A file under `deploy/`, `web/`, the `Dockerfile`, or a root pnpm file runs the root workspace and the suites that name it, because Rust tests read these files by path.
- A path that no package owns and no rule names selects every applicable workspace and suite.
- Documentation-only changes select nothing, including quality, and exit successfully.
  `tests/selection.toml` defines documentation as `docs/`, `.beads/`, and root Markdown files.
  A Markdown file below another directory also follows the package rules above.

Without `--tier`, Cargo first runs each selected workspace.
The root step then runs ignored cases whose `requires:` reasons name only available prerequisites.
Their package and target must have a tier row that matches the selected tiers and is not `kind`.
An `env:NAME` prerequisite needs a nonempty variable after input preparation.
A `bin:PROGRAM` prerequisite needs an executable file on `PATH` or at the named absolute path.
The step runs each eligible case by its exact name in its own package and target.
Other ignored cases stay ignored.
An explicit `--tier` filters executable targets within each selected package.
Without `--tier`, the tool selects unit, contract, live-postgres, and live-nats.
Use `--tier T[,T]` to select one or more tiers.
The tool builds the guests that selected suites need.
`tools/repo-lint` also compiles guest targets that disable Cargo tests.
A passing workspace test result does not establish compilation of those guest targets.
If the change set selects a guest of the host image, `tools/test-changes` also runs `docker build --target component-builder --build-arg WAMN_CHECKOUT_KEY="$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)"`.
These guests are the packages that the Dockerfile `component-builder` stage builds, so a shared WIT root or a platform crate that a guest links selects them.
The stage checks the router pin, as the guest build of `wamn build` does.
Control-client tests compose this pinned raw guest with an empty dispatcher and link operation calls without authority.
`tools/test-changes --tier kind` runs no guest build.
`tests/selection.toml` owns suite commands, tiers, prerequisites, and inputs beyond Cargo dependencies.
Generator changes select `package_digest` in `wamn-package-control`.
Platform fixture and catalog changes select `wamn-control-provision`.
SQL changes select `wamn-ctl` with `ops`, including `protected_relations_live`.
Control and platform deployment changes select the exact native host case.
Every change outside documentation selects the prekind suite, which runs `environment_render` and `platform_render` in `wamn-environment-control`.
The release-chart-render suite runs the ignored Helm cases in `release_chart_render`.
These contract targets compile separately from the kind cases.
Web changes select repository web lint, runtime tests, and platform UI checks, tests, and build.
Every change outside documentation selects `tools/repo-lint run`, including root formatting and Clippy for every target.

The tool builds native binaries, application packages, and guests only when selected suites need them.
The selection file names each preparation step, its output variables, and its commands.
The runner keeps nonempty overrides and skips their preparation commands.
The pinned HTTP guest keeps its required release artifact. Other native and standalone guest inputs use debug mode.
It builds the three HTTP workflow guests under `$CARGO_TARGET_DIR/no-std`.
It copies those debug guests to the root target path that the native host test reads.
It builds applications through a private PostgreSQL database.
Root workspace targets compile without `apps/target/wamn`.
The quality suite still needs generated application files because Clippy compiles the apps workspace.
The tool derives those application roots from Cargo metadata and builds each base before its overlays.
It exports `WAMN_APPLICATION_COMPONENTS` and the selected guest and host paths.
Control and CLI suites enable `ops`.
Runtime changes also select the ignored JetStream round trip.
Runtime and host changes also select the exact native host case.
The tool starts a private NATS server on a free loopback port and stops it at exit.
Set `WAMN_NATIVE_C_NATS_BIN` to a prebuilt `nats-server`, or put `nats-server` on `PATH`.
If neither exists, the tool fails and names the variable.
It never downloads or compiles NATS.

Use `--suite ID[,ID]` to run named suites on any tree, including an unchanged tree.
This selects only those suites and their preparation steps.
Run the Receiving and WMS proof from the repository root:

```bash
tools/test-changes --suite receiving-business,wms-local
```

Append `dry-run` to print the preparation commands and test filters without execution.
The runner executes suites that declare `ignored = true` with `--ignored`.
Every suite test command passes through `tools/require-test-result`.
Only selector discovery uses `--list`, which executes no tests.

For direct Cargo commands or artifact comparisons, set these overrides explicitly.
The runner also accepts them and preserves every nonempty value.
In the paths below, `TARGET` means `CARGO_TARGET_DIR`, and `ROOT` means this checkout.

| Override | Default input |
| --- | --- |
| `WAMN_NATIVE_C_NATS_BIN` | `nats-server` |
| `WAMN_APPLICATION_COMPONENTS` | `TARGET/virtualized/std-empty-environment` |
| `WAMN_EDGE_DEVICE_COMPONENT` | `ROOT/apps/target/wamn/edge_device/components/device.wasm` |
| `WAMN_JSONATA_COMPONENT` | `ROOT/apps/target/wamn/wamn_wms/components/jsonata.wasm` |
| `WAMN_EFFECTIVE_RELEASE_BASE_COMPONENT_WASM` | `ROOT/apps/target/wamn/platform_fixture/components/fixture.wasm` |
| `WAMN_EFFECTIVE_RELEASE_OVERLAY_COMPONENT_WASM` | `ROOT/apps/target/wamn/platform_fixture_overlay/components/fixture_overlay.wasm` |
| `WAMN_GENERATED_WIDGET_UPDATE_SCHEMA` | `ROOT/apps/target/wamn/platform_fixture/routes/widget/update.json` |
| `WAMN_HOST_BINARY` | `TARGET/debug/wamn-host` |
| `WAMN_RECEIVING_DEV_BIN` | `TARGET/debug/wamn-ctl` |
| `WAMN_IDENTITY_BINARY` | `TARGET/debug/wamn-identity` |
| `WAMN_FLOW_HTTP_COMPONENT` | `TARGET/apps/wasm32-wasip2/release/http_route.wasm` |
| `WAMN_MATERIALIZER_COMPONENT` | `TARGET/apps/wasm32-wasip2/debug/materializer.wasm` |
| `WAMN_BLOB_PUT_COMPONENT` | `TARGET/apps/wasm32-wasip2/debug/blob_put.wasm` |
| `WAMN_SQLX_TRANSACTION_COMPONENT` | `TARGET/apps/wasm32-wasip2/debug/sqlx_command.wasm` |
| `WAMN_PLATFORM_UI_DIST` | `ROOT/apps/platform_ui/dist` |
| `WAMN_SYNC_BENCH_DIST` | `TARGET/sync-bench/web` |
| `WAMN_HOST_LIVE_NATS_SERVER_BIN` | Same input as `WAMN_NATIVE_C_NATS_BIN` |
| `WAMN_RECEIVING_DEV_HOST_BIN` | Same input as `WAMN_HOST_BINARY` |
| `WAMN_SESSION_EXCHANGE_CTL_BIN` | Same input as `WAMN_RECEIVING_DEV_BIN` |
| `WAMN_DEV_ENV_FLOW_HTTP_COMPONENT` | Same input as `WAMN_FLOW_HTTP_COMPONENT` |
| `WAMN_DEV_ENV_MATERIALIZER_COMPONENT` | Same input as `WAMN_MATERIALIZER_COMPONENT` |
| `WAMN_POSTGRES_BIN_DIR` | `/usr/lib/postgresql/18/bin` |

The PostgreSQL directory holds `initdb`, `postgres`, `pg_ctl`, and `psql` from PostgreSQL 18.
An empty override uses the default directory.
The `operator_inputs` table in the selection file documents credentials, external addresses, comparison artifacts, and parent-owned fixture inputs.
The runner does not create those operator inputs.

Other cross-package file readers still need explicit path rules.
`tools/test-changes dry-run` prints the selected commands.
`tools/test-changes run` reports each step and returns a nonzero status if any command fails.
Each required command must report at least one executed passing case and exit successfully.
Missing packages, targets, and exact test names fail before any test runs.
The same rule applies to contract-diff, owned delivery, and the upstream release tests.
`tools/require-test-result` captures command output in a private temporary file, prints it after completion, and removes the file.
It preserves command failures and refuses absent or entirely ignored test results.
An explicit dry run or a change set with no selected tests remains a planning result.
It records no permanent result.
`tools/test-changes` is the broad check before integration, and it runs once per merge.
Every merge runs the install render checks in the prekind suite.
`tools/test-revision` runs it on a committed revision.
Implementation work runs only fmt, Clippy and the tests of the packages that it changed before handoff.
Merges are fast-forward after reviewer acceptance.

`tools/test-revision REF [--base BASE]` tests a committed revision in a fresh detached worktree at `/var/tmp/wamn-test-revision/worktree`.
Its default base is the merge base of `REF` and `origin/main`.
If `origin/main` is absent, it uses local `main`.
If that worktree already exists, the tool refuses and names its path.
It initializes submodules and reuses `$HOME/.cache/wamn-target/revision` between runs.
The fixed worktree path keeps compiled source paths valid between runs.
It stores logs in a temporary directory under `/var/tmp`.
If the selected suites need web packages, it runs `pnpm install --offline` there.
Before the test stage, it runs `cargo run --offline -p wamn-submodule-check -- main` with initialized submodules.
It then runs `tools/test-changes run --base BASE` in that worktree.
It prints a PASS or FAIL table with UTC start and end times for each step.
After success, it removes the worktree and keeps the logs and Cargo target directory.
It prints the path of the retained logs.
After failure, it prints the retained worktree path and keeps its logs and build files.
It uses committed files from `REF`, so commit tool changes before testing them through this runner.
Run `tools/test-revision-test` for offline self-tests with fake external commands.

The client compatibility test reads the current generated widget update schema at run time.
Its preparation step builds the platform fixture and supplies the schema path.

`tools/test-changes --tier kind` selects the kind targets of affected packages.
It runs ignored cases one at a time.
The generated exclusion tests in `apps/Cargo.toml` need PostgreSQL diagnostics, so each runs through its own example.
The `exclusion_diagnostics` example of `wamn-test-infrastructure` runs the platform fixture test, and the `exclusion_diagnostics` example of `wamn-receiving-tests` runs the Receiving test.
[Ignored tests](#ignored-tests) gives the commands.
`--name FILTER` requires `--tier kind`.
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

Exclusive resource tests use private PostgreSQL databases and the prebuilt NATS server at `WAMN_NATIVE_C_NATS_BIN`.
The typed call test also needs Docker, the `registry:2` image, and the generated `modbus_reader` component.
It tests replacement connection Y after disabling X, plus two hosts sharing X for the 30-second refusal.
The rollout test drives two host owners in one process through the host shutdown function and a real SIGTERM.
It serves the native `/readyz` endpoint while the second host waits for its lease.
Run these tests with the build environment in [building](building.md):

```bash
cargo test --locked --offline -p wamn-runtime resource_lease::tests -- --test-threads=1
cargo test --locked --offline -p wamn-environment-control --test integration_restart_live -- --ignored --test-threads=1
cargo test --locked --offline -p wamn-host --bin wamn-host host::lease_tests::sigterm_drains_calls_then_hands_the_lease_to_a_ready_host -- --ignored --exact --test-threads=1
```

The following commands select individual test targets:

```bash
cargo test --locked --offline -p wamn-schema-generator --test platform_generation
cargo test --locked --offline -p wamn-wms-tests --lib
cargo test --manifest-path apps/Cargo.toml --locked --offline \
  -p receiving --all-targets
```

The application READMEs identify additional tests and binary owners.
Use `tools/repo-lint dry-run` to see its exact commands without executing them.
`tools/repo-lint run` runs formatting and Clippy across all three workspaces.
It then runs dependency boundaries and the repository policy lints of the `repo-policy` binary.
It reports each command and returns a nonzero status if any command fails.
The full schema files own fresh platform installations.

Before integration, `tools/test-revision` runs the submodule guard with Git history and initialized submodules.
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
The fixture starts a private NATS with TLS and replicates the host auth cache from its `wamn-auth` bucket.
Select `receiving-business,wms-local` with the runner described above.
The Receiving suite runs `route_authentication_live::local_business::*` in its library target.
The WMS suite runs `local_*` in its library target, including its browser cases.
The runner prepares application components, the pinned HTTP guest, and the prebuilt NATS executable before either suite.
These cases need no registry, cluster, check binary, or container image.
The WMS browser cases also need Node and Chrome on `PATH`.
The `sync-bench` step installs web dependencies offline and builds the screen after WMS application generation.

The publish composition, edge bundle, release digest, and control-client cases use the same prepared HTTP guest.
Their preparation rows supply the edge component and application packages when selected.
The release publication case starts a private registry.
The control-client cases also need the installed web dependencies.

The edge route, device and forward tests run in the workspace run.
Each test binary builds the edge release bundle of [`apps/edge_device`](../../apps/edge_device/README.md) once, when its first test starts.
The `edge` preparation step supplies its application component.
The HTTP guest must match its pin.
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

The SQLx transaction case uses the `sqlx-command` preparation step.
It commits and rolls back commands under the current PostgreSQL role.

## Prekind render and native host tests

These cases use the environment release chart without a cluster.
The render cases need Helm 4 on `PATH`.
The `native-host` suite also needs PostgreSQL 18 and a prebuilt NATS executable.
Its preparation rows build the host, HTTP guest, edge package, and HTTP workflow guests.
The runner starts a private PostgreSQL database for application generation.

The case resolves rendered Secrets through the production credential provisioner and starts private PostgreSQL and NATS servers.
It starts the debug host with rendered arguments and local component files.
It also sends an upload to the separate file listener and requires the answer of the native file handler.
This release serves no application host routes, so the handler answers `route-capacity-exhausted`.
It requires readiness, the rendered HTTP workload, and a completed manual HTTP workflow with the expected response.
Its old `respond` terminal must fail with `respond-without-caller` after the successful workflow.
That failure proves that rendering alone cannot establish workflow success.
The case prints its elapsed time and removes its temporary databases, processes, and component files.

Run the environment render cases by their full names:

```bash
for name in prekind_environment_renders prekind_rejects_old_missing_conn_reader \
  prekind_receiving_pat_renders prekind_wms_pat_renders; do
  cargo test --locked --offline -p wamn-environment-control --test environment_render \
    "$name" -- --exact --nocapture --test-threads=1 || exit "$?"
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

## Execution tiers

`tests/selection.toml` assigns every executable Cargo target to exactly one tier.
Cargo metadata defines executable targets through `test = true`.
The strongest boundary in a target determines its tier.
A target with no tests takes unit.

- Unit covers pure decisions and controlled time. It uses no database, broker, network service, subprocess, or component runtime.
- Contract covers serialized values, WIT, generated artifacts, rendered objects, files, command-line tools, and owned loopback fixtures.
- Live-postgres requires private PostgreSQL behavior.
- Live-nats requires a real NATS broker. A test that also requires PostgreSQL takes this tier.
- Kind requires Kubernetes.

Unit and contract run per issue, live tiers run per merge, and kind runs per epic.
`platform_apply_kind` runs once at each epic close and once on `main` before a GCP build.
It never runs for a single fix.
The timeout in the selection file stops a stage after 30 minutes of silence.
It gives no expected duration.
The epoch interruption case lives in `wamn-engine --test deadline`.
Paused-time scheduling decisions live in `wamn-workflow --test schedule_decisions`.
Admission authority stays in `wamn-run-state --test admission_live`.
Schedule storage stays in `wamn-environment-control --test workflow_schedule_live`.
Broker acknowledgement lives in `wamn-environment-control --test workflow_event_nats`.
The native host case lives in `wamn-dev-control --test native_host` and takes live-nats.

## Ignored tests

An ignore reason lists only prerequisites in the fixed form `requires: bin:<program>, env:<VARIABLE>`, with comma-separated entries.
The test checks every listed program and nonempty environment variable before setup, and missing prerequisites fail the test.
The tier lives only in `tests/selection.toml`.

To run kind targets in selected packages, use `tools/test-changes --tier kind`.
They run one at a time.
To select tests by name, add `--name FILTER`, as [select the relevant tests](#select-the-relevant-tests) describes.

Run the generated exclusion test of the platform fixture through its PostgreSQL fixture:

```bash
cargo run --locked --offline -p wamn-test-infrastructure --example exclusion_diagnostics
```

The fixture starts disposable PostgreSQL, applies the platform fixture migration, and adds a test-only exclusion constraint on `widget`.
It captures actual update failures under a non-superuser role.
It regenerates the platform fixture code in a temporary copy.
Then it runs the ignored test `data::error::tests::generated_update_exclusion_from_postgres` of `fixture`.
`WAMN_EXCLUSION_DIAGNOSTICS` supplies the server diagnostics to that test.
The fixture removes its database, generated copy, diagnostics, and build output when it finishes.
The application schemas and generated files in the checkout do not change.

Run the generated exclusion test of Receiving through its own PostgreSQL fixture:

```bash
cargo run --locked --offline -p wamn-receiving-tests --example exclusion_diagnostics
```

The Receiving fixture does the same steps with the Receiving migration and a test-only exclusion constraint on `purchase_order`.
Then it runs the ignored test `data::error::tests::generated_update_exclusion_from_postgres` of `receiving`.

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

### Acceptance runs

Owner ruling 2026-11-05: the non-kind selection of an acceptance run always runs to completion.
It does not stop at the first red. Every step runs.
Report every red together in one report, with its log lines.
Do all the diagnosis that the reds need in the same pass, without further approval.
This diagnosis includes single-case reruns that keep their artifacts and builds of the same case at the base commit.
The result is one run, one complete list of findings, and one ruling session.
A kind stage or another cluster stage still stops at its first red.

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
The kind cases stay ignored, so this command starts no cluster.
`--no-fail-fast` retains later binary results after a failure.

The sweep runs no [ignored test](#ignored-tests), because each one needs inputs that a general test run does not have.
The ignored tests run in [the live stage](#the-live-stage).
`--nocapture` shows the output of each test.

Run selected kind tests separately with `tools/test-changes --tier kind run`.
Tell other sessions before the run because concurrent clusters overload the machine.
Receiving and WMS retain their native business cases in the live-nats tier.

Record the start and end time of each run, and time its build apart from its tests.

Keep the sweep log outside the repository.
Set `WAMN_RESULTS` outside the checkout, so that the run writes nothing into the tree.
Cite the path of `$WAMN_RESULTS/workspace.log` and the commit that the sweep ran on in the close reason of the epic.
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
| `[RUN-PLANE-RECONCILE]` | `crates/control/environment/tests/run_plane_live.rs`: covers creation, current no-op, two-plane residency, platform configuration, and current security behavior under the process lock |
| `[CLAIMS-LIVE]` | Runtime `plugins::wamn_postgres::claims::tests` `live_*` cases: run by default on the test server, and the cases that create fixed roles hold the process lock |
| `[EFFECT-SNAPSHOT]` | `crates/platform/runtime/tests/effect_snapshot_live.rs`: ignored in live-nats, starts its own server with `pg_stat_statements`, and needs the NATS inputs below. It runs the production cluster snapshot as a provisioned HTTP admitter, holds the process lock, and makes sure that a repeated snapshot sends no statement. |
| `[R18-NEG]` | Runtime `plugins::wamn_postgres::claims::tests::live_scs_off_server_fails_checkout_closed`: runs by default on a separate server started with `standard_conforming_strings=off` |
| `[EVT-READER]` | `services/cdc-reader/tests/event_reader_live.rs`: ignored in live-nats, starts its own server with `wal_level=logical`, and needs `WAMN_READER_NATS_URL` |
| `[CAPTURE-GAP]` | `crates/control/environment/tests/capture_gap_live.rs`: ignored, starts its own server with `wal_level=logical`, and needs `WAMN_READER_NATS_URL`. It does not test the refusal of an active slot, because only a streaming reader holds a slot active |
| `[SQLX-TRANSACTION]` | `crates/platform/runtime/tests/sqlx_transaction_live.rs`: ignored in live-postgres, takes its database from the test server, and needs `WAMN_SQLX_TRANSACTION_COMPONENT` |
| `[MGMT-LIVE]` | `services/scenario-worker/tests/management_live.rs`: runs by default on the test server and holds the process lock |
| `[STD-GUEST-VIRTUALIZATION]` | `tests/integration/src/virtualized_std_guest.rs`: ignored in live-nats, and needs built guest files and its explicit `WAMN_STD_VIRTUALIZATION_*` inputs |
| `[EVT-C-CDC]` | `tests/integration/src/cdcbench.rs`: runs through `wamn-gates cdcbench`, starts its own server with `wal_level=logical`, and needs a JetStream NATS at `--nats-url` |

The source owners declare additional credentials, artifact paths, and selected assertions.
Do not substitute shared services for missing test inputs.
For deployed runs, use [cluster commands](cluster-tests.md).

The broker cases in `auth_drops_live`, `connection_drops_live`, and `effect_snapshot_live` use the platform NATS client.
Set `WAMN_NATS_URL`, `WAMN_NATS_TLS_CA`, `WAMN_NATS_TLS_CERT`, and `WAMN_NATS_TLS_KEY` for a private TLS broker.
Each case creates its memory buckets through the production function and needs permission to create and delete keys.
The [delivery fixture](#local-saved-edit-acceptance) supplies an owned broker and development certificates.
Inside its child command, use `WAMN_RECEIVING_DEV_NATS_URL` for `WAMN_NATS_URL`.
Use its `WAMN_DEV_ENV_NATS_TLS_CA`, `WAMN_DEV_ENV_NATS_TLS_CERT`, and `WAMN_DEV_ENV_NATS_TLS_KEY` for the corresponding platform variables.

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

The case applies a fixture package through `wamn-ctl apply-package` and runs `wamn-ctl-ops prune-record-history`.
`prune_removes_only_the_expired_prefix_of_each_row` also creates the run retention generation to test the refusals.

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
The `identity` preparation step builds the test identity binary with `test-util`.
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

Use the preparation rows for the native programs, application packages, HTTP guest, and materializer.
From the disposable worktree, build its fixture example and list its test executable:

```bash
cd "$SOURCE"
cargo build --locked --offline -p wamn-receiving-tests --example delivery_timings
cargo test --locked --offline -p wamn-receiving-tests --lib --no-run
```

Set `WAMN_LOCAL_TEST_BINARY` to the absolute executable path printed by the last command.
Set `WAMN_LOCAL_RESULTS` to an unused private directory outside the worktree.
Run the exact case through the owned fixture:

```bash
WAMN_LOCAL_DEV_EDIT_ROOT="$SOURCE" \
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
The fixture's NATS admits the materializer consumers of the packages that the command gives `wamn dev up`.
These are `wamn_receiving` and `client_acme_receiving`, or `wamn_wms` with `--wms`.
The fixture sets the CDC reader binary and the event publisher credential for `wamn dev up`.
It also sets the event materializer credential, which the broker scopes to the source stream.
It sets `WAMN_DEV_ENV_ROOT` to `delivery-timings/env` in the target directory, which survives runs, so the compile cache survives too.
Before the command starts, the fixture counts the files under `dev-wasmtime-cache/modules` in that root, without the `.stats` files.
It writes the count to `cache-state.json` in the results directory, so that you can read a cold run as cold.
`wamn-test-postgres` starts its server with `wal_level=logical`.
It supplies the service coordinates that the case declares.
For direct fixture commands, set the artifact overrides from the table above.
The case starts its own PostgreSQL server, because the development environment resets the control store of the whole server.
The case requires authenticated application results after code, SQL, and schema edits.
It requires retained data after code and query edits.
In reset mode, a changed physical SQL fingerprint recreates the whole recorded disposable target.
In migrate mode, the watcher reports pending SQL. Explicit schema apply owns migration effects and records.
Invalid SQL refuses preparation before replacement.
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

These tests drive product commands and observe the deployed cluster.
Select each test by its exact name in the `platform_apply_kind` Cargo target:

| Test | Coverage | Platform |
| --- | --- | --- |
| `platform_install_recovery_kind` | Install, sign-in, reapply, pod recovery, release repair, interrupted application installation and replacement recovery | Creates one fresh kind cluster |
| `application_deploy_kind` | Source and published code updates, reapply, rollback, missing artifacts, stale checkout, retained data, and independent applications | Attaches to an existing disposable kind platform |
| `sql_job_kind` | SQL before and after code, migration checksums, retry without repeated data effects, both mode transitions, and matching adoption | Attaches to an existing disposable kind platform |
| `same_name_reset_kind` | Source and published schema reset, unchanged input, interruption before and after recreation, same-name retry, durable refusal, and environment isolation | Attaches to an existing disposable kind platform |
| `platform_apply_kind` | Fresh-platform acceptance: normal installation and interrupted installation once each, plus all focused bodies on the normal platform | Creates two fresh kind clusters |

The focused application and schema tests never install or rebuild the platform.
Each invocation creates unique project, environment, hostname, and package-version names.
These tests can run in any order on the same platform.
They retain application environments until the disposable platform is deleted.
Platform and application recovery checks run only in the platform tests.
Stale-event admission has no test because deployed environments have no workflow authoring operation (`wamn-alrvy.19`).
Reset still asserts old-event disposal and environment isolation.
`wamn-alrvy.20` (temporary directory leak) and `wamn-alrvy.22` (uncovered lint requirements) remain open.
The accepted Epic 2 items `wamn-xy404`, `wamn-1riwn`, and `wamn-c361p` remain open.
`wamn-azv06` remains not reproduced. `wamn-fszfy`, `wamn-m4wnp`, and `wamn-ib8ej` also remain open.
These test descriptions do not claim persistent system database upgrades or GCP coverage.


All tests require Docker, kind, Helm, kubectl, OpenSSL, and a prepared KCL compiler selected by `WAMN_KCL`.
Build the native `wamn` and `wamn-ctl` binaries beside the test executable in `$CARGO_TARGET_DIR/debug`.
Use prepared artifacts from [local platform production](building.md#local-platform-artifacts).
Keep the complete prepared chart directory and its pinned images unchanged after installation.
The application bodies build scratch copies of `apps/platform_fixture` through product commands.
All tests therefore also require these publication inputs:

- Set `DATABASE_URL` to a separate disposable PostgreSQL 18 server with permission to create and drop databases.
- Set `WAMN_FLOW_HTTP_COMPONENT` to the raw pinned `http_route.wasm`.
- Set `WAMN_PACKAGE_ARTIFACT_BASE`, `WAMN_COMPONENT_ARTIFACT_BASE`, and `WAMN_RELEASE_ARTIFACT_BASE` under the selected kind registry.
- Set `WAMN_REGISTRY_AUTH_FILE` to the registry's protected `.dockerconfigjson` file.

The registry authority must match `cluster.k`, normally `localhost:5001`.
Tests read product-created service credentials from Secrets.
Control routes require a password-login session and refuse a PAT (personal access token).
Application routes use an owner PAT issued through identity's operator endpoint.

Supply `WAMN_TEST_OWNER_PASSWORD` for the existing platform's owner before the first focused run.
The first run enrolls an owner without a password through `/password/enroll`, with the product's local invitation.
It then signs in through `/password/session` for the control audience.
Later runs observe the stored password credential and skip enrollment without reading or consuming another invitation.
They obtain the same password from the caller's `WAMN_TEST_OWNER_PASSWORD` and sign in again.
Tests never print or persist this password and create no saved-step file.
Fresh-platform tests generate one password in memory and reuse it across their focused bodies.

Install once with the product command from a shell with the prepared chart and publication inputs:

```bash
export PATH="$CARGO_TARGET_DIR/debug:$PATH"
export WAMN_KCL="$(tools/install-kcl)"
export WAMN_RELEASE_CHART=/var/tmp/wamn-platform-chart
cargo build --locked --offline -p wamn-ctl --bin wamn --bin wamn-ctl
mkdir -p /var/tmp/wamn-kind-dep3
sed 's/cluster_name = "wamn-local"/cluster_name = "wamn-test-dep3"/' \
  deploy/kind/cluster.k > /var/tmp/wamn-kind-dep3/cluster.k
wamn platform deploy /var/tmp/wamn-kind-dep3/cluster.k --chart "$WAMN_RELEASE_CHART"
kind export kubeconfig --name wamn-test-dep3 --kubeconfig /var/tmp/wamn-kind-dep3/kubeconfig
kubectl --kubeconfig /var/tmp/wamn-kind-dep3/kubeconfig --context kind-wamn-test-dep3 \
  annotate namespace platform wamn.dev/disposable=true
```

The namespace annotation records consent to use this platform as disposable.
Focused tests refuse an absent or false annotation before any platform mutation.
`WAMN_TEST_CLUSTER` is the single target input authorized by `wamn-alrvy.18`.
The cluster document supplies the name for the existing provider's kubeconfig export and the product's `Target`.
Helm's `platform/chart` value supplies the installed chart path, regardless of the caller's `WAMN_RELEASE_CHART`.

Before the first focused run, enter the owner password without echoing it:

```bash
read -r -s -p 'Disposable owner password: ' WAMN_TEST_OWNER_PASSWORD
printf '\n'
export WAMN_TEST_OWNER_PASSWORD
```

Keep that password in your normal secret store.
In later shells, load the same value into `WAMN_TEST_OWNER_PASSWORD` from that store.
If the owner already has a password, supply that existing password.
Run one focused SQL test against that platform:

```bash
WAMN_TEST_CLUSTER=/var/tmp/wamn-kind-dep3/cluster.k \
  cargo test --locked --offline -p wamn-dev-control --test platform_apply_kind sql_job_kind -- --ignored --exact --nocapture
```

Select `application_deploy_kind` or `same_name_reset_kind` in the same command for the other focused tests.
Run fresh-platform acceptance with the prepared chart selected by `WAMN_RELEASE_CHART`:

```bash
cargo test --locked --offline -p wamn-dev-control --test platform_apply_kind platform_apply_kind -- --ignored --exact --nocapture
```

A CLI-only change rebuilds the native binaries and the affected test executable.
It reuses the installed platform and its prepared chart and images.
An in-cluster deployment change requires a new ctl image.
Host, identity, and role-component changes require their affected images.
Produce those artifacts with `build-platform`, then install another disposable platform through the product command.
Never overwrite the prepared chart of an existing platform.
A fixture change rebuilds the affected application artifacts through the test's product publication path.

Fresh-platform tests delete their clusters after success.
Failed runs retain private inputs and logs and print cleanup commands.
Focused tests never delete the attached platform.
After its focused runs finish, delete the reusable platform:

```bash
kind delete cluster --name wamn-test-dep3
```

The journeys have no saved completion state or resume system.
Retry product deployment with the same cluster document and prepared chart.

### Removed application cluster coverage

DEP2-02 removes the Receiving and WMS cluster suites and their duplicate installers.
It also removes the RC runner and the separate identity/session cluster journey.
Their deleted qualification cases have no placeholders.

The historical Receiving cluster combinations covered command histories, membership revocation, postcommit delivery, and materialization.
The WMS combinations covered deployed routes, label failures after commit, delivery, and host restart with compiled code reuse.
The combined identity journey covered key rotation, TLS, session targets, and membership changes.
The RC runner combined bootstrap with socket and trace Jobs.
These historical combinations receive no replacement in Epic 2.
DEP2-13 supplies focused coverage through product deployment commands.

Native application, identity, session, socket, and trace tests remain.
Shared business fixtures, registry transport, and image archive tests remain.
Surviving render cases call the product renderer.
The database tests in `install_sequence_live.rs` retain restricted SQL roles, package grants, and invitation refusal coverage.

### Environment deployment coverage

The focused tests above split the DEP2-13 application journey by contract.
Fresh-platform acceptance calls those same bodies and keeps platform recovery separate from schema tests.
DEP2-09 already removed `environment_reapply.rs`, its synthetic chart, and its direct Helm write and interruption helpers.
The old connector, file, drain, deletion, and schedule combinations have no replacement in this epic.
Product rendering tests and the retained native host, file fixture, and database tests remain at their existing tiers.

### Files on PostgreSQL and MinIO

The C4 tests use disposable PostgreSQL databases. Compile them without starting services:

```bash
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='kache test-runner'
cargo test --locked --offline -p wamn-environment-control --test files_live --test platform_grants_live --no-run
cargo test --locked --offline -p wamn-control-provision --test family_denial_matrix --test deploy_sql_authority --no-run
cargo test --locked --offline -p wamn-execution-host --test files_live --test host_route_live --no-run
cargo test --locked --offline -p wamn-schema-introspection --test file_reference_live --no-run
```

The NATS variable selects an existing server binary and prevents a native NATS build.

Run the live cases with Docker and the PostgreSQL binaries:

```bash
cargo test --locked --offline -p wamn-environment-control --test files_live --test platform_grants_live -- --nocapture
cargo test --locked --offline -p wamn-control-provision --test family_denial_matrix --test deploy_sql_authority -- --nocapture
cargo test --locked --offline -p wamn-execution-host --test files_live --test host_route_live -- --nocapture
cargo test --locked --offline -p wamn-schema-introspection --test file_reference_live -- --nocapture
```

The file fixture declares read and write S3 bindings, including a binding for a store without a CDN host.
Its document migration references `platform.file(id)`.
The metadata tests cover grants, stamps, history, connection disablement, and referenced deletion refusal.
Registry tests use real scoped logins for `registry.lookup_file_connection`.
They refuse unmapped logins, forged scope, disabled connections, and projects without enablement.
They also read the recorded generation after a bucket and prefix change.

The native host tests start an owned MinIO container and disposable PostgreSQL servers.
They require PostgreSQL 18 binaries at `/usr/lib/postgresql/18/bin` and a running Docker daemon.
They also require the pinned MinIO and mc images in `support/files.rs`.
The fixture removes its containers and servers after each test.
The tests upload more than 64 MiB through a stream, compare hashes, and repair committed staging bytes.
They enforce sessions, operation grants, file ownership, private exposure, and read binding refusals.
Signed URL tests call `FileRoutes::signed_url`, then require 403 after expiry and a 900-second lifetime cap.
The package fixture also declares a public projection with a three-second signed read result.
The test generates its contracts and admits its Wasm component through the production package admission path.
It invokes that operation through `RouterDeliveryBridge` as a session caller that holds only the operation grant.
The returned URL serves the private bytes, then returns 403 after expiry. Direct `file.get` returns 403.
This test needs no external host, guest build, NATS server, or additional environment variables.
Run that case alone with the exports above, Docker, and the PostgreSQL binaries:

```bash
cargo test --locked --offline -p wamn-execution-host --test files_live package_operation_returns_a_private_file_signed_url_that_expires -- --exact --nocapture
```

Run its package admission test without PostgreSQL or MinIO:

```bash
cargo test --locked --offline -p wamn-execution-host --test files_live package_signed_read_admits_the_real_node_abi_and_manifest_owned_authority -- --exact
```

Public exposure tests require anonymous origin reads, the cache header, and origin 404 after unexpose.
Raw blobstore mutations refuse the reserved private and public keys.

The age test imports a month-old metadata fixture through a non-superuser table owner.
It restores the stamp trigger in the same transaction before the test actions.
It seeds aged objects through the `x-minio-source-mtime` header and reads their actual timestamps.
The [pinned MinIO request parser](https://github.com/minio/minio/blob/76c06ff1d2cc844050fdeb1d54bf8fa70db72278/cmd/object-api-options.go) defines that header.
The test installs a one-day lifecycle rule for staging only, then requires that live bytes and unlinked metadata stay.
No clock change or external aged snapshot is required.

The retained `file_fixture_live` test assembles the file package on disposable PostgreSQL without kind.
It requires the `blob-put` component and component virtualizer named in its prerequisite declaration.
It asserts that the release has an HTTP role and an authored HTTP route.

```bash
cargo test --locked --offline -p wamn-environment-control --test file_fixture_live \
  native_files::file_fixture_assembles_an_http_role_without_kind -- --ignored --exact
```

### Registry image archive in kind

This test runs `case_release::image_archive` of `wamn-gate-harness` against a private registry with a login and TLS.
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
One test composes Acme over Receiving into a copy of their build output.
It requires the composed component to embed the exact bytes of the Receiving component.
The application preparation step supplies both packages.

### Generated TypeScript bindings

The client emitter tests run the emitted bindings through the web runtime and compile them with the workspace TypeScript compiler.
Run `pnpm install --offline` at the repository root first.
The compilation test skips with a message only when the workspace `tsc` executable is absent.

```bash
cargo test --locked --offline -p wamn-schema-generator --test client_ts_emitter
```
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
It copies them into `src/generated/wamn_control` and `src/generated/wamn_control_org` of each package that names them in `package.json` `"wamn": { "clients": [...] }`.
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

Use the preparation rows for the native programs, application components, and platform UI.
Build the fixture and test before the timed run:

```bash
TMPDIR=/var/tmp cargo build --offline -p wamn-receiving-tests --example delivery_timings
TMPDIR=/var/tmp cargo test --offline -p wamn-ctl --test platform_ui_live --no-run
```

The fixture needs Docker and its local service images, PostgreSQL 18 binaries, and Chrome.
It uses the Playwright installation in `web/shell`, so run `pnpm install` at the repository root first.
`CARGO_TARGET_DIR` must name the directory of the built native programs.
Run this command from the repository root:

```bash
export TMPDIR=/var/tmp
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
The receipt goes through the sync receipt panel, and the browser waits until the status line shows "Pending: 0. Refused: 0."
It also creates a supplier and uses the form cancel action.
The browser requires table-header actions, full-panel tables, literal uppercase breadcrumbs, and forms that return to their tables.

Build the test without starting services:

```bash
TMPDIR=/var/tmp cargo test --offline -p wamn-ctl --test receiving_ui_live --no-run
```

For direct fixture commands, set the artifact overrides from the table above.
Run from the repository root:

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

### Local Receiving sync

`local_receiving_sync` runs five browser checks against the Receiving sync screens.
It uses the same build steps, variables, and fixture as [Local Receiving screens](#local-receiving-screens).
The checks are:

1. A 30 second network drop: the screens make no page read and no navigation.
2. A second browser records a receipt, and the first browser shows the new remaining quantity within one poll.
3. The browser reopens offline with a saved session, shows "Offline, sign in to upload.", and replays the receipt when the network returns.
4. Eight network flaps with three receipts: each receipt uploads once.
5. A refused receipt goes to the "Not uploaded" region, and a corrected copy uploads.

Run from the repository root:

```bash
export TMPDIR=/var/tmp
results="$(mktemp -d /var/tmp/wamn-receiving-sync-XXXXXX)/run"
"$CARGO_TARGET_DIR/debug/examples/delivery_timings" "$PWD" "$CARGO_TARGET_DIR" "$results" -- \
  cargo test --offline -p wamn-ctl --test receiving_ui_live \
  local_receiving_sync -- --ignored --exact --nocapture
```

The test prints "CHECK n PASS" for each check and then "PASS: Receiving sync checks".
Failure screenshots go to `<results>/receiving-sync-screenshots/`.
The service log stays at `<results>/receiving-sync-env/control-web.log`.

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

The package-build PostgreSQL fixture and the local package registry use `docker rm -f -v`.
For a retained platform cluster, use the cleanup command printed by its test.
Manual platform cleanup keeps the shared build registry, as [deployment](deployment.md#cleanup) describes.
These commands also remove anonymous volumes, which Docker creates for image-declared data directories.
Container restarts retain their data until cleanup removes the container.
Inspect their recorded cleanup result and remaining resource names before reporting completion.
For manual fixtures, remove only the exact container, cluster, image, volume, and private directory that your run created.

A killed cluster test (SIGKILL) does not run its cleanup, so its cluster and containers stay.
After a killed run, find the cluster that your run created with `kind get clusters`.
For a historical Receiving or WMS cluster, use the shared Rust fixture:

```bash
cargo run --locked --offline -p wamn-test-infrastructure --bin wamn-test-cluster -- <cluster>
```

The fixture deletes the kind cluster and its owned containers, registry volume, and local image tags.
Never remove a cluster that another session created.

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
The focused `platform_apply_kind` cases own product-command observations of deployed startup, readiness, authenticated requests, and cleanup.

### Workflow schedules

The schedule task's unit tests run with paused time: the first tick after a restart, a row enabled or disabled between ticks, and ticks missed during a slow admission.

```bash
cargo test --locked --offline -p wamn-workflow --test schedule_decisions
```

The live test authors schedule workflows on a disposable PostgreSQL 18 database, admits due moments, and drains the runs through the real queue.
One case runs the host's schedule task in real time: a row enabled after the task starts fires at its next tick, and a disabled row fires at no later tick.
Another holds a lock on the rows across one minute: the late tick admits its own due moment, the missed tick is skipped, and the next tick fires.
Each of these two cases takes up to about three minutes.

```bash
cargo test -p wamn-environment-control --test workflow_schedule_live -- --nocapture
```

### Connection candidates

Run the candidate route test with PostgreSQL 18, the local NATS binary, Docker, and the wasm32-wasip2 target available.
Set `WAMN_NATIVE_C_NATS_BIN` to that NATS binary.
The test starts private database, broker, registry, TLS, and Modbus fixtures.
It tests good and bad candidates, version selection, cached compilation, authority refusal, and unchanged generation and credential counts.

```bash
cargo test --locked --offline -p wamn-execution-host --test control_route_live --features wamn-runtime/wasm_component_model_implements,wash-runtime/washlet connection_test::candidate_http_and_modbus_health_commit_nothing -- --ignored --exact --nocapture
```
