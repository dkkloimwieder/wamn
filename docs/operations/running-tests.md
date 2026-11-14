# Running tests

Run commands from the repository root unless a command changes directories.
[Testing strategy](../testing/strategy.md) defines what each test method establishes.
[Building](building.md) lists the toolchain and build prerequisites.

Before a test run, prepare the checkout once from its repository root:

```bash
eval "$(cargo xtask lane-start "$PWD")"
```

The script initializes submodules and selects the Cargo cache directory.
It runs no builds, web installation, or database.
It exports `CARGO_TARGET_DIR` and `WAMN_CHECKOUT_KEY`.
Cargo output lies in `$CARGO_TARGET_DIR`, so the commands below use that path and not `target/` or `apps/target`.
`wamn build` still writes package output to `apps/target/wamn`.
After the package build, `cargo xtask test-changes` runs `wamn web build` for each selected application with `web/package.json`.
The standalone web artifacts supply browser files for the developer session tests.
Selected tests supply their own preparation from `tests/selection.toml`.

## Select the relevant tests

Run only relevant existing tests for a behavior change or a concrete failure.
For mechanical edits, compare source, paths, and syntax.
Do not repeat builds or tests without a new behavior change or concrete failure.
Cleanup boundaries do not require broad test runs.

`cargo xtask test-changes` selects suites and builds their inputs before tests run.
The change set is the working tree against `git merge-base HEAD main`, or against the commit that `--base REF` names.
It includes committed, staged, unstaged, deleted, and untracked files.

- A file in a Cargo package selects that package and its dependents in every workspace that resolves it: normal and build dependents transitively, then one development dependent.
- A file under `apps/<name>/` outside a package selects every package under that directory in every workspace, with their dependents.
- A workspace `Cargo.toml` or `Cargo.lock` runs that whole workspace.
- Any `Cargo.toml` or `Cargo.lock` also runs `cargo xtask compile-packages-alone`, which runs `cargo check -p` for each root workspace package on its own. A package that compiles only through a feature that another package turns on fails it.
- Canonical interfaces live under repository-root `wit/`. Router and runtime interfaces under `wit/` select all three workspaces because bindings read WIT outside Cargo dependencies.
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
The step runs each eligible case by its exact name from the test executable that Cargo already built.
It preserves the complete feature selection of that build and starts no second Cargo build.
A selected ignored suite owns its named cases or filter, so the root step leaves those cases to that suite.
Other ignored cases stay ignored.
An explicit `--tier` filters executable targets within each selected package.
Without `--tier`, the tool selects unit, contract, live-postgres, and live-nats.
Use `--tier T[,T]` to select one or more tiers.
The tool builds the guests that selected suites need.
`cargo xtask repo-lint` also compiles guest targets that disable Cargo tests.
A passing workspace test result does not establish compilation of those guest targets.
The platform producer builds both role components through the package component builder.
`tests/selection.toml` owns suite commands, tiers, prerequisites, and inputs beyond Cargo dependencies.
Platform fixture and catalog changes select `wamn-control-provision`.
SQL changes select the environment retention and protected-relation interfaces with `ops`.
Control and platform deployment changes select the exact native host case.
Every change outside documentation selects `environment_render` and `platform_render` in `wamn-environment` through prekind or Cargo selection.
The release-chart-render suite runs the ignored Helm cases in `release_chart_render`.
Path rules add to the selected targets and cases.
The runner combines feature requirements for targets that both Cargo paths and suites select.
A target runs through Cargo selection or its suite once for each execution mode: ordinary or ignored.
These contract targets compile separately from the kind cases.
Web changes select repository web lint, runtime tests, and platform UI lint, tests, and build.
Full-workspace lint is an explicit integration operation:

```bash
cargo xtask test-changes --suite quality
```

The quality suite runs `cargo xtask repo-lint run` without application preparation.
Prepare generated application inputs through their owning build commands before full-workspace Clippy.

The tool builds native binaries, application packages, and guests only when selected targets or suites need them.
The selection file names each preparation step, its output variables, and its commands.
The runner keeps nonempty overrides and skips their preparation commands.
The pinned HTTP guest keeps its required release artifact. Other native and standalone guest inputs use debug mode.
It builds the three HTTP workflow guests under `$CARGO_TARGET_DIR/no-std`.
It copies those debug guests to the root target path that the native host test reads.
It builds applications through a private PostgreSQL database.
Root workspace targets compile without `apps/target/wamn`.
Preparation bindings name their packages and executable targets in the selection file.
The runner matches bindings against selected Cargo targets and suites that run a whole target.
Exact or filtered suites declare their own prerequisites.
Application preparation builds each base before its overlays.
Web consumers share one workspace installation per selection.
The runner skips that installation when no selected consumer needs it.
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
The database executor suite needs no application, browser, device, or platform preparation:

```bash
cargo xtask test-changes --suite package-executor
```

This suite runs `wamn-package-control`'s `migrations_live` target.
It covers SQLx commit and checksum behavior, history write failures, and the restricted migration login.
A source diff still selects dependent packages and their integration inputs.
Run the Receiving and WMS proof from the repository root:

```bash
cargo xtask test-changes --suite receiving-business,wms-local
```

Append `dry-run` to print the preparation commands and test filters without execution.
The runner executes suites that declare `ignored = true` with `--ignored`.
Every suite test command passes through `cargo xtask require-test-result`.
Only selector discovery uses `--list`, which executes no tests.

For direct Cargo commands or artifact comparisons, set these overrides explicitly.
The runner also accepts them and preserves every nonempty value.
In the paths below, `TARGET` means `CARGO_TARGET_DIR`, and `ROOT` means this checkout.

| Override | Default input |
| --- | --- |
| `WAMN_NATIVE_C_NATS_BIN` | `nats-server` |
| `WAMN_APPLICATION_COMPONENTS` | `TARGET/wasm32-wasip2/release` |
| `WAMN_EDGE_DEVICE_COMPONENT` | `ROOT/apps/target/wamn/edge_device/components/device.wasm` |
| `WAMN_JSONATA_COMPONENT` | `ROOT/apps/target/wamn/wamn_wms/components/jsonata.wasm` |
| `WAMN_EFFECTIVE_RELEASE_BASE_COMPONENT_WASM` | `ROOT/apps/target/wamn/platform_fixture/components/fixture.wasm` |
| `WAMN_EFFECTIVE_RELEASE_OVERLAY_COMPONENT_WASM` | `ROOT/apps/target/wamn/platform_fixture_overlay/components/fixture_overlay.wasm` |
| `WAMN_GENERATED_WIDGET_UPDATE_SCHEMA` | `ROOT/apps/target/wamn/platform_fixture/routes/widget/update.json` |
| `WAMN_HOST_BINARY` | `TARGET/debug/wamn-host` |
| `WAMN_RECEIVING_DEV_BIN` | `TARGET/debug/wamn-ctl` |
| `WAMN_IDENTITY_BINARY` | `TARGET/debug/wamn-identity` |
| `WAMN_FLOW_HTTP_COMPONENT` | `TARGET/apps/wasm32-wasip2/release/http_route.wasm` |
| `WAMN_BLOB_PUT_COMPONENT` | `TARGET/apps/wasm32-wasip2/debug/blob_put.wasm` |
| `WAMN_SQLX_TRANSACTION_COMPONENT` | `TARGET/apps/wasm32-wasip2/debug/sqlx_command.wasm` |
| `WAMN_PLATFORM_UI_DIST` | `ROOT/apps/platform_ui/dist` |
| `WAMN_SYNC_BENCH_DIST` | `TARGET/sync-bench/web` |
| `WAMN_HOST_LIVE_NATS_SERVER_BIN` | Same input as `WAMN_NATIVE_C_NATS_BIN` |
| `WAMN_RECEIVING_DEV_HOST_BIN` | Same input as `WAMN_HOST_BINARY` |
| `WAMN_SESSION_EXCHANGE_CTL_BIN` | Same input as `WAMN_RECEIVING_DEV_BIN` |
| `WAMN_DEV_ENV_FLOW_HTTP_COMPONENT` | Same input as `WAMN_FLOW_HTTP_COMPONENT` |
| `WAMN_POSTGRES_BIN_DIR` | `/usr/lib/postgresql/18/bin` |

The PostgreSQL directory holds `initdb`, `postgres`, `pg_ctl`, and `psql` from PostgreSQL 18.
An empty override uses the default directory.
The `operator_inputs` table in the selection file documents credentials, external addresses, comparison artifacts, and parent-owned fixture inputs.
The runner does not create those operator inputs.

Other cross-package file readers still need explicit path rules.
`cargo xtask test-changes dry-run` prints the selected commands.
`cargo xtask test-changes run` reports each step and returns a nonzero status if any command fails.
Each required command must report at least one executed passing case and exit successfully.
Missing packages, targets, and exact test names fail before any test runs.
The same rule applies to the component contract and upstream release commands.
`cargo xtask require-test-result` streams child output and checks the libtest summary after completion.
It preserves command failures and refuses absent or entirely ignored test results.
An explicit dry run or a change set with no selected tests remains a planning result.
It records no permanent result.
`cargo xtask test-changes` is the broad test before integration, and it runs once per merge.
Every merge runs the install render tests in the prekind suite.
Implementation work runs only fmt, Clippy and the tests of the packages that it changed before handoff.
Merges are fast-forward after reviewer acceptance.

The client compatibility test reads the current generated widget update schema at run time.
Its preparation step builds the platform fixture and supplies the schema path.

`cargo xtask test-changes --tier kind` selects the kind targets of affected packages.
It runs ignored cases one at a time.
When an exact kind suite selects the aggregate journey, that suite owns the target and the root command omits it.
A name filter that selects focused cases instead omits the aggregate suite.
The selection runs the aggregate journey or its focused cases once.
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
cargo test --locked --offline -p wamn-environment --test integration_restart_live -- --ignored --test-threads=1
cargo test --locked --offline -p wamn-host --bin wamn-host host::lease_tests::sigterm_drains_calls_then_hands_the_lease_to_a_ready_host -- --ignored --exact --test-threads=1
```

The following commands select individual test targets:

```bash
cargo test --locked --offline -p wamn-compiler --test platform_generation
cargo test --locked --offline -p wamn-wms-tests --lib
cargo test --manifest-path apps/Cargo.toml --locked --offline \
  -p receiving --all-targets
```

The application READMEs identify additional tests and binary owners.
Use `cargo xtask repo-lint dry-run` to see its exact commands without executing them.
`cargo xtask repo-lint run` runs formatting and Clippy across all three workspaces.
It then runs dependency boundaries and `repo-policy` for file-location conventions and version identities.
It reports each command and returns a nonzero status if any command fails.
The full schema files own fresh platform installations.

Before integration, `cargo xtask test-changes` runs the submodule guard with Git history and initialized submodules.
Changed pins require a commit subject that names the submodule path or its last segment and says `bump` or `pin`, plus `revert` when the pin does not descend from main.

## What a test asserts

A test stays only if a realistic defect fails it, and nothing else rejects that defect more strongly.
The expected value of the test must not come from the code under test.

- No test compares source text, DDL text, or a generated file to a string.
- A byte or digest test stays only when the bytes are a persisted or wire contract and the expected value is independent.
- A source or repository policy test is a lint in `cargo xtask repo-lint`, not a test.
- A schema rule is tested against the installed database: a refused write, a cascade, or a `pg_catalog` fact.
- A new live test must not repeat an existing one. Extend the existing test when a rule has no other test.

## Local application business tests

Receiving and WMS retain their business assertions.
The in-process application assembler is removed.
These scenarios need a product deployment fixture on the development cluster before they can run.
The native host and private provisioning fixtures no longer supply their prerequisites.

The publish composition, edge bundle, release digest, and control-client cases use the same prepared HTTP guest.
Their preparation rows supply the edge component and application packages when selected.
The release publication case starts a private registry.
The control-client cases also need the installed web dependencies.

The edge route, device and forward tests run in the workspace run.
Each test binary builds the edge release bundle of [`apps/edge_device`](../../apps/edge_device/README.md) once, when its first test starts.
The `edge` preparation step supplies its application component.
The HTTP guest must match its pin.
The bundle writer stores the route guest separately. The edge invocation switch follows in S7-D.
The package bundle writer admits the component and writes the bundle without a development session.
The tests sign their own sessions.
The device test writes scale frames into a pseudo-terminal pair, which is its virtual serial port.
The forward test runs the `wamn-edge` binary as a child, kills it with SIGKILL twice, and serves an HTTPS platform with its own certificate authority.

```bash
cargo test --locked --offline -p wamn-edge --test route --test device --test forward
```

The test `samples_reach_a_live_platform_once_across_kills` forwards to a real platform: the `sample.record` route of [`apps/edge_samples`](../../apps/edge_samples/README.md) on a [developer session](development-loop.md).
Deploy `apps/edge_samples` through the product development command.
Use its HTTPS application address. Write the caller token to a file with mode 0600.
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

## Deployed connection and caller tests

`connection_and_caller_kind` uses an installed disposable platform.
It publishes fixture source and deploys it through authenticated control routes.
It requires an HTTP connection to reach an installed upstream service.
A manual workflow that ends in `respond` must refuse with `respond-without-caller`.
The test observes the final run state in the deployed database.

```bash
cargo test -p wamn-dev --test platform_apply_kind connection_and_caller_kind -- --ignored --exact --nocapture
```

Use the same cluster document and prepared artifacts as the other focused platform tests.

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

Minimum setup functions create a database with the schema that a test starts from.
Each lives beside the crate that owns its SQL, behind that crate's `test-util` feature.
Enable the feature on a dev-dependency.
The minimums do not take the lock.

| Minimum | Function | Installs |
| --- | --- | --- |
| system | `wamn_control_provision::test_database::system()` | the control store, applied as `wamn_system`, which owns the database |
| project | `wamn_catalog::test_database::project()` | `CATALOG_SCHEMA_SQL` and the `wamn_app` role |
| project+platform | `wamn_platform_identity::test_database::project_platform()` | the project base grants and `deploy/sql/app-schema.sql` |

A test of provisioning, migrations, or grants executes that operation and does not start from a minimum that already performed it.
Exercise the intended role because a superuser bypasses row security.

An [ignored test](#ignored-tests) takes its database from the same functions.

For Docker fixtures, choose a new container name and an unused loopback port.
Make sure that a real query succeeds through the same connection path that the command uses.

The `wamn-test-postgres` runner binary in `wamn-test-infrastructure` serves delivery tooling and the manual generation commands below.
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
Both runners use the shared process boundary in `cargo xtask require-test-result`. `WAMN_TEST_SILENCE_SECONDS` sets its silence limit, with a default of 1800 seconds. Output on either stream resets the limit. Silence past the limit stops the child process group, names the stage, and fails the run.
It gives no expected duration.
The epoch interruption case lives in `wamn-engine --test deadline`.
Pure calendar scheduling decisions live in `wamn-workflow --test schedule_decisions`.
Admission authority stays in `wamn-run-state --test admission_live`.
`wamn-run-state --test run_state_live` covers atomic admission, lease recovery, fencing and retention.
Live schedule cases call the service with its real clock and can take several minutes.
Schedule storage stays in `wamn-environment --test workflow_schedule_live`.
Broker acknowledgement lives in `wamn-environment --test workflow_event_nats`.
The connection and caller case lives in `wamn-dev --test platform_apply_kind` and requires an installed development cluster.

## Ignored tests

An ignore reason lists only prerequisites in the fixed form `requires: bin:<program>, env:<VARIABLE>`, with comma-separated entries.
The test requires every listed program and nonempty environment variable before setup, and missing prerequisites fail the test.
The tier lives only in `tests/selection.toml`.

To run kind targets in selected packages, use `cargo xtask test-changes --tier kind`.
They run one at a time.
To select tests by name, add `--name FILTER`, as [select the relevant tests](#select-the-relevant-tests) describes.

Run the generated exclusion test of the platform fixture through its PostgreSQL fixture:

```bash
cargo build --locked --offline -p xtask
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
cargo build --locked --offline -p xtask
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
cargo test --workspace --locked --offline --features wamn-environment/ops \
  --no-fail-fast -- --nocapture --test-threads=1 \
  > "$WAMN_RESULTS/workspace.log" 2>&1
```

Record its exit status before running another command.
Do not pipe the command through `tail` or another output filter.
`--workspace` selects all root members, including members outside Cargo's defaults.
`--features wamn-environment/ops` builds the targets that require the `ops` feature.
The kind cases stay ignored, so this command starts no cluster.
`--no-fail-fast` retains later binary results after a failure.

The sweep runs no [ignored test](#ignored-tests), because each one needs inputs that a general test run does not have.
The ignored tests run in [the live stage](#the-live-stage).
`--nocapture` shows the output of each test.

Run selected kind tests separately with `cargo xtask test-changes --tier kind run`.
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
cargo xtask contract-diff run > "$WAMN_RESULTS/contract-diff.log" 2>&1
```

It runs the guest `http-route` adversarial tests.
The guest test belongs to another workspace, so the root sweep cannot reach it.
`cargo xtask contract-diff dry-run` only prints the commands.

Report failures, ignored cases, explicit skips, filtered cases, and executed passes separately.
Report each failure's package, target, full test name, and actual cause.
A changed test name does not establish a new failure or remove an earlier failure.

## The live stage

The live stage runs named ignored tests with their inputs set, apart from the full sweep.
A cluster day or a live test cites the live stage on its bead.
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
| `[IDENTITY-LIVE]` | Runtime `plugins::wamn_postgres::session_identity::tests` `live_*` cases: run by default on the test server, and the cases that create fixed roles hold the process lock |
| `[R18-NEG]` | Runtime `plugins::wamn_postgres::session_identity::tests::live_scs_off_server_fails_checkout_closed`: runs by default on a separate server started with `standard_conforming_strings=off` |
| `[EVT-READER]` | `services/cdc-reader/tests/event_reader_live.rs`: ignored in live-nats, starts its own server with `wal_level=logical`, and needs `WAMN_READER_NATS_URL` |
| `[CAPTURE-GAP]` | `crates/control/environment/tests/capture_gap_live.rs`: requires a Kubernetes target, a stamped release chart, PostgreSQL 18 with `wal_level=logical`, and the platform NATS inputs below. It stops and restarts the reader Deployment during recovery |
| `[SQLX-TRANSACTION]` | `crates/platform/runtime/tests/sqlx_transaction_live.rs`: ignored in live-postgres, takes its database from the test server, and needs `WAMN_SQLX_TRANSACTION_COMPONENT` |

The source owners declare additional credentials, artifact paths, and selected assertions.
Do not substitute shared services for missing test inputs.
For deployed runs, use [cluster commands](cluster-tests.md).

The broker cases in `auth_drops_live` and `connection_drops_live` use the platform NATS client.
Set `WAMN_NATS_URL`, `WAMN_NATS_TLS_CA`, `WAMN_NATS_TLS_CERT`, and `WAMN_NATS_TLS_KEY` for a private TLS broker.
Each case creates its memory buckets through the production function and needs permission to create and delete keys.
Use the installed disposable platform's broker and its normal platform NATS inputs.
The [development loop](development-loop.md) does not provision a private broker.

`production_operator_authentication_and_operation_authorization` keeps `identity.log` and `test.log` in a new `wamn-route-auth-*` directory under `${TMPDIR:-/var/tmp}`.
A failure prints both paths.

### Record history retention test

`crates/control/environment/tests/history_retention_live.rs` tests record retention through the owning library.
It creates roles and holds the process lock.

```bash
cargo test --locked --offline -p wamn-environment --features ops --test history_retention_live -- --nocapture
```

The cases assert retained rows, credential refusals and database grants.

### Run history retention test

`crates/control/environment/tests/prune_terminal_run_history_live.rs` tests terminal run retention through the owning library.
It creates roles and holds the process lock.

```bash
cargo test --locked --offline -p wamn-environment --features ops --test prune_terminal_run_history_live -- --nocapture
```

The cases assert retained runs, credential refusals and database grants.

### Development on kind

The development loop uses the installed identity and control services.
[Development loop](development-loop.md) gives the source publication and deployment commands.
The focused connection and caller test above uses this product path.

The CLI browser targets `platform_ui_live` and `receiving_ui_live` also use this path.
They call `wamn env apply` and `wamn dev` against an installed disposable platform.
Supply `WAMN_TEST_CLUSTER`, `WAMN_DEV_ENV`, `WAMN_TIMINGS_ROOT`, and the existing `WAMN_BROWSER_*` sign-in inputs listed in each test.
The environment directory contains `environment.k` for one application.
The platform browser reads invitation mail from the installed identity service's outbox.
Receiving browser assertions still require the fixed business dataset. The product has no import route for that dataset.

`wamn-integration-tests --test application_routes_kind` tests batch retry, independent batch outcomes, health requests, read limits, sequence bindings, and overlay actor stamps through deployed routes.
It uses `WAMN_CTL_BIN` for the product binary and the publication inputs from the development procedure.
Supply the cluster, environment directory, browser base URL, and `WAMN_SYSTEM_ADMIN_URL` named in its prerequisite list.
The test creates its own environment through control and uses SQL only to observe results.
It removes the deployed environment after success. Its database remains for explicit data cleanup.

`wamn-receiving-tests --test receiving_routes_kind` checks purchase-order numbers and caller stamps.
It creates its supplier through the public create route.
`wamn-wms-tests --test wms_browser_kind` runs the saved-request and shell-update browser cases against deployed WMS routes.
It creates locations and packaging through HTTP. It uses SQL only to observe the resulting rows.
Supply `WAMN_SYNC_BENCH_DIST`, Node.js, and Chrome in addition to the shared platform inputs.
The environment template must select the application's prepared connection bindings.
Each fixture copies those bindings through the connection routes into its own environment.
The browser adapters accept HTTP and HTTPS endpoints, including their application path prefix.

### Platform and environment apply on kind

These tests drive product commands and observe the deployed cluster.
Select each test by its exact name in the `platform_apply_kind` Cargo target:

| Test | Coverage | Platform |
| --- | --- | --- |
| `control_operations_kind` | Authenticated Job credentials, cross-project denial without a Job, two-application selection isolation, rollback, schema mutation and artifact worker | Creates one fresh kind cluster; bounded to 45 minutes |
| `platform_install_recovery_kind` | Interrupt platform installation after its first Kubernetes write, retry, status, and owner sign-in | Creates one fresh kind cluster |
| `application_recovery_kind` | Interrupted explicit provisioning and code installation, replacement recovery, canonical reapply, retained data, and application isolation | Attaches to an existing disposable kind platform |
| `platform_upgrade_kind` | Independent control and identity updates, one application host update, code reapply, earlier runtime selection, and retained data | Attaches to an existing disposable kind platform |
| `platform_schema_kind` | One system file and one selected project file through SQL Jobs, unchanged application rows and releases, and second-environment isolation | Attaches to an existing disposable kind platform |
| `same_name_reset_kind` | Two initial application releases, data, published schema-change reset, interrupted same-name CNPG reset, reconnection, durable refusal, and environment isolation | Attaches to an existing disposable kind platform |
| `platform_apply_kind` | Fresh-platform acceptance: normal and interrupted installation, plus application recovery and reset bodies on the normal platform | Creates two fresh kind clusters |

The focused application and schema tests never install or rebuild the platform.
Each focused application invocation creates a unique project, hostnames, and package versions.
The aggregate journey prepares each immutable artifact once and reuses it across its focused bodies.
Each body starts with an absent environment and installs only its required initial releases.
The reset body installs no earlier code versions.
Artifact preparation uses the product build and publication operation before the focused bodies run.
These tests can run in any order on the same platform.
The recovery body provisions the environment with `environment::apply` and initializes SQL with `schema apply` before code deployment.
The reset, platform runtime, and platform SQL bodies use those same explicit commands.
Code reapply reads the canonical selection through `status`.
Platform recovery publishes no applications.

After a body succeeds, product lifecycle cleanup removes its environment, database, and roles.
Reset and platform schema cases also delete their isolation environments after all assertions succeed.
Failed bodies retain their resources and logs.
The old environment worker and its terminal confirmation are removed.
Creation and deletion journeys need the replacement lifecycle routes before they can run.
Tests must use those routes for setup and cleanup.

`wamn-package-control/tests/migrations_live.rs` covers SQLx checksums, transaction rollback, retry behavior, and failed history writes.
It also calls package apply to refuse rebinding an existing relation to another package or entity.
The same target checks direct migration credentials and refuses administrator sessions for platform migrations.
`wamn-compiler/tests/build_live.rs::history_changes_append_immutable_versions_and_execute_during_build` covers generated history versioning.
These live targets require PostgreSQL. Compile them without starting a server with `cargo test -p <package> --test <target> --no-run`.
Workflow authoring now has an authenticated control operation. Its deployed acceptance coverage remains with the workflow owner.
Reset still asserts old-event disposal and environment isolation.
`wamn-alrvy.20` (temporary directory leak) and `wamn-alrvy.22` (uncovered lint requirements) remain open.
The accepted Epic 2 items `wamn-xy404`, `wamn-1riwn`, and `wamn-c361p` remain open.
`wamn-azv06` remains not reproduced. `wamn-fszfy`, `wamn-m4wnp`, and `wamn-ib8ej` also remain open.
The platform schema case covers explicit system and project SQL upgrades on kind.
These tests provide no GCP coverage.


All tests require Docker, kind, Helm, kubectl, OpenSSL, and a prepared KCL compiler selected by `WAMN_KCL`.
Focused application bodies and fresh-platform acceptance also require `script` for product deletion confirmation.
Build the native `wamn` and `wamn-ctl` binaries beside the test executable in `$CARGO_TARGET_DIR/debug`.
Use prepared artifacts from [local platform production](building.md#local-platform-artifacts).
Keep the complete prepared chart directory and its pinned images unchanged after installation.
Artifact preparation builds authored scratch packages from `apps/platform_fixture` through product commands.
Focused application bodies and aggregate acceptance also require these publication inputs:

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
export WAMN_KCL="$(cargo xtask install-kcl)"
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
Run focused application recovery against that platform:

```bash
WAMN_TEST_CLUSTER=/var/tmp/wamn-kind-dep3/cluster.k \
  cargo test --locked --offline -p wamn-dev --test platform_apply_kind application_recovery_kind -- --ignored --exact --nocapture
```

Select `same_name_reset_kind` in the same command for reset and CDC tests.
Run fresh-platform acceptance with the prepared chart selected by `WAMN_RELEASE_CHART`:

```bash
cargo test --locked --offline -p wamn-dev --test platform_apply_kind platform_apply_kind -- --ignored --exact --nocapture
```

DEP6's bounded case uses one fresh platform, trusted product commands for its first environment,
and authenticated control routes for the subsequent deploy, rollback, and schema mutation.
It reads actual Job pods, the typed applied configuration through `wamn-registry`, and application release ConfigMaps.
It deletes its publishing checkout before route execution.
It reads the worker's image, immutable chart inputs, and storage mounts to exclude paths from the submitting machine.
Kind has no web bucket, so hosted platform UI serving is verified on GCP in DEP7.
Supply all publication inputs listed above before the run.
Allow 30–45 minutes with prepared platform artifacts.
The test enforces a 45-minute limit that includes setup and successful cleanup.
Run it with this command:

```bash
cargo test --locked -p wamn-dev --test platform_apply_kind \
  control_operations_kind -- --ignored --exact --nocapture
```

The case deletes its cluster after success and retains failed resources with exact cleanup commands.
It creates another project for the denied caller.
Cluster deletion removes both projects.
Project deletion still has no standalone product operation.

A CLI-only change rebuilds the native binaries and the affected test executable.
It reuses the installed platform and its prepared chart and images.
An in-cluster deployment change requires a new ctl image.
Host, identity, and role-component changes require their affected images.
Produce those artifacts with `cargo xtask build-platform`, then install another disposable platform through the product command.
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

#### Focused platform schema preparation

Use the disposable platform, owner password, and publication inputs described above.
Keep chart A, the installed chart, available and unchanged.
In a scratch source tree, add one SQL file under each of `deploy/sql/migrations/system/` and `deploy/sql/migrations/project/`.
Each file must create one probe table and insert one row.
Keep the initialization SQL and existing migration files unchanged.
Rebuild and publish only the `control` image from that tree.
Use `cargo xtask build-platform` to stamp chart B with that image and every other image from chart A. Install the selected control worker configuration through the platform owner.
Set `WAMN_RELEASE_CHART` to chart B.

DEP5-06 adds `platform_schema_kind` with its body in `support/platform_schema.rs`.
The case refuses equal `control` digests or a changed digest for any other image.
It derives pending file names and checksums from `wamn platform schema diff` for chart B.
It reads each database catalog directly and requires one new table with one row in the applied database, and no relation change in the others.
The existing journey deploys one application release into two environments and deletes both environments after success.
The case needs explicit owner approval to run.
Approval must cover setup, both SQL applies, and cleanup on the disposable platform.

#### Focused platform upgrade preparation

DEP4-06 prepares `platform_upgrade_kind` in `crates/control/dev/tests/platform_apply_kind.rs`.
Its body lives in `support/platform_upgrade.rs` beside the existing application journey.
The test compiles separately from execution.
The test stays ignored until its externally prepared cluster, second chart, and owner password are supplied.
The setup, cleanup, and upgrade operations still need separate owner approval for cluster execution.
Allow 20 to 30 minutes, including application preparation and service restoration.
This duration is an estimate, not a measured result.

Use the disposable platform and publication inputs described above.
Keep its installed chart directory available.
Set the existing `WAMN_RELEASE_CHART` variable to a second prepared chart.
That chart must select distinct immutable HTTP, identity, and host images.
Publish those images before the run.
The test builds no container image and never replaces the installed default chart.

The existing application journey prepares two immutable application releases before the upgrade body.
It creates a unique project through the control route.
It deploys beta and alpha through `wamn deploy --release`.
It creates counter and widget rows through each application's routes.
The upgrade body uses only the prepared charts and published release digests.
It runs no reset, migration, recovery matrix, or aggregate acceptance case.

Make sure that setup works independently before you approve the upgrade body.
Run `wamn platform status "$WAMN_TEST_CLUSTER"` against the disposable platform.
Make sure that the platform services report ready.
Use `helm get values` to make sure that every recorded chart remains readable.
Use the existing `wamn schema publish` producer to prepare the two scratch application releases.
Deploy each published digest into a fresh environment with `wamn deploy --release DIGEST --env FILE`.
Use `wamn status --env FILE` to observe distinct ready application releases.
Create and read one counter and widget through each application's routes.
Retain their identifiers for the cleanup observation.
These operations show that setup works without a platform upgrade.

Make sure that cleanup works on that environment before you approve the upgrade body.
Remove the environment through its product lifecycle owner.
Enter only the disposable environment's coordinate at its confirmation prompt.
Observe that its database, roles, and Helm releases disappear.
Make sure that the shared CNPG Cluster stays ready.
Make sure that the other platform resources remain ready.
The existing journey supplies the same terminal confirmation through `script`.
The upgrade body restores its original control and identity charts before environment deletion.
The test removes its scratch checkout and private directory after success.
Failures retain resources, private inputs, and logs for diagnosis.
The test never deletes the attached platform.
Project deletion remains unsupported by the product.

After setup and cleanup pass, obtain approval for this exact command:

```bash
CARGO_TARGET_DIR=/home/kaalin/.cache/wamn-target/dep4 \
  cargo test --locked --offline -p wamn-dev --test platform_apply_kind platform_upgrade_kind -- --ignored --exact --nocapture
```

Supply `WAMN_TEST_CLUSTER`, `WAMN_TEST_OWNER_PASSWORD`, `WAMN_KCL`, and the publication inputs from the preceding instructions.
Supply the second chart through `WAMN_RELEASE_CHART`.
The case observes running image digests, Helm revisions, pods, credentials, signing rows, scheman identity, and application data.
It leaves setup, cleanup, readiness, and data retention unverified until their approved cluster runs pass.
Automatic rollback after a failed rollout stays unverified even after this case passes.
This case explicitly reselects an earlier image and does not induce a rollout failure.

### Removed application cluster coverage

DEP2-02 removes the Receiving and WMS cluster suites and their duplicate installers.
It also removes the RC runner and the separate identity/session cluster journey.

The historical Receiving cluster combinations covered command histories, membership revocation, postcommit delivery, and materialization.
The WMS combinations covered deployed routes, label failures after commit, delivery, and host restart with compiled code reuse.
The combined identity journey covered key rotation, TLS, session targets, and membership changes.
The RC runner combined bootstrap with socket and trace Jobs.
These historical combinations receive no replacement in Epic 2.
DEP2-13 supplies focused coverage through product deployment commands.

Native application, identity, session, socket, and trace tests remain.
Shared business fixtures, registry transport, and image archive tests remain.
Surviving render cases call the product renderer.
The native installation sequence test is removed with the private development provisioner.

### Environment deployment coverage

The focused tests above split the DEP2-13 application journey by contract.
Fresh-platform acceptance calls those same bodies and keeps platform recovery separate from schema tests.
DEP2-09 already removed `environment_reapply.rs`, its synthetic chart, and its direct Helm write and interruption helpers.
The old connector, file, drain, deletion, and schedule combinations have no replacement in this epic.
Product rendering, file fixture, and database tests remain at their existing tiers.

### Files on PostgreSQL and MinIO

The C4 tests use disposable PostgreSQL databases. Compile them without starting services:

```bash
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='kache test-runner'
cargo test --locked --offline -p wamn-environment --test files_live --test platform_grants_live --no-run
cargo test --locked --offline -p wamn-control-provision --test family_denial_matrix --test deploy_sql_authority --no-run
cargo test --locked --offline -p wamn-execution-host --test files_live --test host_route_live --no-run
cargo test --locked --offline -p wamn-schema-introspection --test file_reference_live --no-run
```

The NATS variable selects an existing server binary and prevents a native NATS build.

Run the live cases with Docker and the PostgreSQL binaries:

```bash
cargo test --locked --offline -p wamn-environment --test files_live --test platform_grants_live -- --nocapture
cargo test --locked --offline -p wamn-control-provision --test family_denial_matrix --test deploy_sql_authority -- --nocapture
cargo test --locked --offline -p wamn-execution-host --test files_live --test host_route_live -- --nocapture
cargo test --locked --offline -p wamn-schema-introspection --test file_reference_live -- --nocapture
```

The file fixture declares read and write S3 bindings, including a binding for a store without a CDN host.
Its document migration references `platform.file(id)`.
The metadata tests cover grants, stamps, history, connection disablement, and referenced deletion refusal.
Registry tests use real targeted logins for `registry.lookup_file_connection`.
They refuse unmapped logins, forged target, disabled connections, and projects without enablement.
They also read the recorded generation after a bucket and prefix change.

The native host tests start an owned MinIO container and disposable PostgreSQL servers.
They require PostgreSQL 18 binaries at `/usr/lib/postgresql/18/bin` and a running Docker daemon.
They also require the pinned MinIO and mc images in `support/files.rs`.
The fixture removes its containers and servers after each test.
Native uploads accept at most 64 MiB. The tests stream uploads, compare hashes, and repair committed staging bytes.
They enforce sessions, operation grants, file ownership, private exposure, and read binding refusals.
Signed URL tests call `FileRoutes::signed_url`, then require 403 after expiry and a 900-second lifetime cap.
The package fixture also declares a public projection with a three-second signed read result.
The test generates its contracts and admits its Wasm component through the production package admission path.
It invokes that operation through the shared operation entry as a session caller that holds only the operation grant.
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
It requires the `blob-put` component named in its prerequisite declaration.
It asserts that the release has an HTTP role and an authored HTTP route.

```bash
cargo test --locked --offline -p wamn-environment --test file_fixture_live \
  native_files::file_fixture_assembles_an_http_role_without_kind -- --ignored --exact
```

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

The compiler describes every emitted statement with SQLx before it emits consumers.
It records parameter types, selected columns, relation access, and transaction requirements in the compiled package document.
The compiler emits no native verifier rows, bind fixtures, or source maps.

### wamn build

The live build tests use the PostgreSQL 18 binaries and start their own server, as the other generator database tests do:

```bash
cargo test --locked --offline -p wamn-compiler --test build_live
```

They copy the platform fixture and build it into their own output root.
A build with an old manifest and obsolete client output must match a build into an empty target.
Both builds must emit the authored operation bindings and components, with identical bytes and `build.json`.
The tests also make sure that a left-out statement type is derived and that the nullability rule of [building](building.md#wamn-build) holds.
One test composes Acme over Receiving into a copy of their build output.
It requires the composed component to embed the exact bytes of the Receiving component.
The application preparation step supplies both packages.

### Generated TypeScript bindings

The client emitter tests run the emitted bindings through the web runtime and compile them with the workspace TypeScript compiler.
Run `pnpm install --offline` at the repository root first.
The compilation test skips with a message only when the workspace `tsc` executable is absent.

```bash
cargo test --locked --offline -p wamn-compiler --test client_ts_emitter
```
To compile the bindings for the platform fixture, run:

```bash
cargo run --locked --offline -p wamn-compiler --example compile_client_ts
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
Every web script that reads the clients writes them first through `cargo xtask host-route-clients`, so you do not need to write them yourself.
To write them without a web script, run:

```bash
cargo run --locked --offline -p wamn-compiler --example materialize_host_route_client
```

`pnpm run lint` in `apps/platform_ui` compiles them, because that package maps `@wamn/control-client` and `@wamn/control-org-client` to them.
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
pnpm run lint
```

The command fails on a lint error but not on a lint warning.
To format the code, run `pnpm run format` at the repository root.

### Web runtime

The runtime is hand-written TypeScript, and its own tests run under Node.
To compile the runtime and its tests, run:

```bash
cd web/runtime && pnpm run lint
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
pnpm --filter @wamn/platform-ui lint
pnpm --filter @wamn/platform-ui test
pnpm --filter @wamn/platform-ui build
```

### Platform sign-in and application screens

Browser sessions use the installed web, identity, and control services.
Use the [development loop](development-loop.md) to publish and deploy an application before opening its screens.

### Generated descriptions

`client_component.rs` emits typed descriptions for `@wfe/generator` from the pinned `web/wfe` package.
The generator integration test compiles fixture descriptions with TypeScript.
It covers table, form, repeated group, detail, and delete descriptions.
Application bindings own requests, validation, and value conversion.

Run `pnpm install --offline` at the repository root first.
The existing manual test uses the same test code:

```bash
TMPDIR=/var/tmp cargo run --offline -p wamn-compiler --example compile_client_components
```

The command writes temporary modules and removes them after the test.
`compile_client_ts` makes sure of the unchanged TypeScript bindings separately.

### Shared Vite tooling and Receiving

`web/shell` keeps the shared Vite helper and the Playwright dependency for browser tests.
The platform UI owns the application shell and its tests.
To compile the helper and the Receiving web application, run:

```bash
pnpm --dir web/shell lint
pnpm --dir apps/wamn_receiving/web lint
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
The historical Receiving/WMS fixture cleanup binary was removed with DEP6-06.
Use the retained run's exact cluster name and the product provider's cleanup commands.
For a run that predates product deployment, inspect and remove only that run's sidecars and image tags.
Never remove a cluster that another session created.

Use explicit cluster names, kubeconfig paths, and contexts for Kubernetes and Helm commands.
Never use `docker prune`, broad image removal, or name-substring cleanup.

Report unresolved cleanup failures and keep only output needed for current work or specifically requested by the user.
Before removing a worktree, preserve its source changes and branch.
Remove only that worktree and its owned target after no process uses them.

## Benchmarks

`cargo xtask bench` runs benchmarks. The `bench-*` commands collect and summarize measurements. SQL replay inputs live in `xtask/bench/`.

## Host and package runtime

Run the combined host's lifecycle tests:

```bash
cargo test --locked --offline -p wamn-host --bin wamn-host host::tests::
```

Queue shutdown recovery and event admission tests use PostgreSQL 18:

```bash
cargo test -p wamn-workflow --lib queue::admission_live::
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
cargo test -p wamn-environment --test workflow_schedule_live -- --nocapture
```

### Connection candidates

Run the candidate route test with PostgreSQL 18, the local NATS binary, Docker, and the wasm32-wasip2 target available.
Set `WAMN_NATIVE_C_NATS_BIN` to that NATS binary.
The test starts private database, broker, registry, TLS, and Modbus fixtures.
It tests good and bad candidates, version selection, cached compilation, authority refusal, and unchanged generation and credential counts.

```bash
cargo test --locked --offline -p wamn-execution-host --test control_route_live --features wamn-runtime/wasm_component_model_implements,wash-runtime/washlet connection_test::candidate_http_and_modbus_health_commit_nothing -- --ignored --exact --nocapture
```
