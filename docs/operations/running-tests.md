# Running tests

Run commands from the repository root unless a command changes directories.
[Testing strategy](../testing/strategy.md) defines what each test method establishes.
[Building](building.md) lists the toolchain and build prerequisites.

## Run the tests

Run these commands in this order:

```bash
eval "$(cargo xtask prepare-tests)"
cargo test --workspace
cargo test --workspace --exclude wamn-journey-tests -- --ignored
```

Both test commands use `--workspace`, because a bare `cargo test` runs only the default members of the root workspace.
On an idle machine, the plain run takes about 60 s after the build. It is slower under load.
Run the third command on a machine with Docker.
The ignored run leaves out `wamn-journey-tests`, because its only test target needs a cluster.
[Development on kind](#development-on-kind) gives the cluster run.

`cargo xtask prepare-tests` builds the test inputs once, before the run.
It builds every application under `apps/` that has a `wamn.k` file.
It builds each base before its overlays, with the committed SQL describe evidence and no database.
The output of each application goes to `apps/target/wamn/<package>`.
If `WAMN_KCL` is not set, the command installs the pinned KCL compiler under `.tools/kcl/`.
It also builds the pinned `ingress` guest from `apps/guests/ingress` and the workflow service from `apps/guests/workflow-service`.

The command then prints one `export` line for each prepared input.
`eval` sets these variables in the current shell:

| Variable | Prepared input |
| --- | --- |
| `WAMN_EFFECTIVE_RELEASE_BASE_COMPONENT_WASM` | `apps/target/wamn/platform_fixture/components/fixture.wasm` |
| `WAMN_EFFECTIVE_RELEASE_OVERLAY_COMPONENT_WASM` | `apps/target/wamn/platform_fixture_overlay/components/fixture_overlay.wasm` |
| `WAMN_EDGE_DEVICE_COMPONENT` | `apps/target/wamn/edge_device/components/device.wasm` |
| `WAMN_INGRESS_COMPONENT` | The release build of the `ingress` guest |
| `WAMN_WORKFLOW_SERVICE_COMPONENT` | The release build of the `workflow-service` guest |

A test whose prepared input is missing fails. It does not skip.
After a source change to an application or to `ingress`, run `prepare-tests` again.

After `prepare-tests`, run `cargo xtask helm-size`.
For each application, it verifies the package and measures the Helm record of its release against the 1 MiB limit.
It fails on a refused package and does not skip it.
[Building](building.md#platform-version) describes the command.

The apps workspace is a separate Cargo workspace.
Run its tests per package, with the commands that [test-changes](#select-the-tests-for-a-change) prints.

### Apps workspace Clippy

Run `prepare-tests` first, because the apps workspace compiles the generated application code.
Then run Clippy on the apps workspace:

```bash
cargo clippy --manifest-path apps/Cargo.toml --locked --workspace \
  --exclude connection-http-standard --all-targets -- -D warnings
```

`cargo xtask repo-lint run` runs this command and the other Clippy variants of both workspaces.
It also runs formatting, cargo-deny and the policy checks in xtask.
It runs `cargo xtask machete` to check both workspaces for unused dependencies.
`cargo xtask repo-lint dry-run` prints each command and runs none.

## Select the tests for a change

```bash
cargo xtask test-changes --base main
```

The command compares the working tree with `--base`. It prints commands and runs no test.
The change set holds four groups of files:

- the files that the commits since the merge base with `--base` change, including deleted files
- the staged files
- the unstaged files
- the untracked files that Git does not ignore

The command reads the Cargo metadata of both workspaces, `Cargo.toml` and `apps/Cargo.toml`.
The owner of a file is the package with the deepest directory that holds the file.
A changed file selects its owner.
A selected package also selects its normal and build dependents, transitively.
Then each package with a development dependency on a selected package is also selected.
Dependents count across both workspaces.

A change to a `Cargo.toml` or a `Cargo.lock` selects every package of its workspace.
A file that no package owns also selects every package of a workspace.
The workspace is the one with the deepest directory that holds the file.
So an unowned file under `apps/` selects the apps workspace, and other unowned files select the root workspace.
A change set with no files prints nothing.

The command prints one line for each selected package. The fields are separated by tabs:

1. the workspace manifest
2. the package name
3. the reason, which names the changed file or the dependency that selected the package
4. the test command, `cargo test --manifest-path <manifest> -p <package>`
5. the same command with `-- --ignored`

A package whose test targets are all `_kind.rs` files gets no fifth field.
That package scope keeps the journey and cluster targets out of the ignored run.
Run `prepare-tests` before the printed commands.

## Test-database isolation

No test connects to an interactive or shared database.
Each test that needs PostgreSQL or NATS starts its own container through `wamn-test-infrastructure`.
The test owns the returned value. Dropping the value removes the container.
There is no shared server, no lock, no `#[sqlx::test]` and no `DATABASE_URL` input.
Tests run in parallel, because each test has its own server.
Roles belong to the whole server, so a private server keeps the fixed product role names apart.

| Helper | Image | Returns |
| --- | --- | --- |
| `wamn_test_infrastructure::containers::start_postgres()` | `postgres.image` | `Postgres` |
| `wamn_test_infrastructure::containers::start_nats(platform, operator_include)` | `nats.container.image.fullImageName` | `Nats` |

At compile time, the helpers read the images from `deploy/charts/platform/values.yaml`.
So the tests use the production digests:

- PostgreSQL: `ghcr.io/cloudnative-pg/postgresql:18.6@sha256:899d3ed526b659d77935dde0e6bf2d69dbbf17d3d8c6486ca8cfd04bd3c18533`
- NATS: `nats:2.12.15-alpine@sha256:b270f5e2428354c0335612694d7dd2fb588148e567a5757fdff325ef9c9332e6`

`render_platform()` renders the platform chart with its default values through `helm template`, so `helm` is a required input.
`start_nats` gives the container the rendered mounts, arguments and environment of the NATS server.
The operator Secret holds `operator_include`, the file that identity bootstrap returns.
Each Certificate Secret holds a new self-signed certificate.
`Nats::connect_options()` requires TLS and trusts only that certificate.
`crates/identity/tests/nats_isolation.rs` holds the NATS isolation tests. Each has the marker `requires: docker, bin:helm`.

The CloudNativePG image has no entrypoint that creates a database.
So the PostgreSQL helper runs its own `initdb` in the container, with these options:

| Option | Reason |
| --- | --- |
| `initdb --no-sync` | `initdb` does not wait for its files to reach the disk. The server is temporary. |
| `fsync=off` | The server does no crash recovery, so it does not force writes to the disk. |
| `full_page_writes=off` | The server does no crash recovery, so WAL holds no full pages. |
| SCRAM-SHA-256 for TCP logins, local socket logins refused | Each login sends a password over the network, as a product client does. |
| `ssl=on` with a new self-signed certificate | The product client requires verified TLS. The URL uses `sslmode=verify-full`. |
| `wal_level=logical`, and a replication line in `pg_hba.conf` | Change capture tests read logical replication. |

Synchronous commit stays on, so logical decoding reads the WAL as soon as a commit returns.
The server listens on a random port of `127.0.0.1`.

`Postgres` gives these functions:

- `admin_url()` gives the administrator URL of the `postgres` database.
- `database_url(database, login, password)` gives a URL for an existing database and login.
- `create_database(name)` creates an empty database and returns its administrator URL.
- `create_system_role()` creates the `wamn_system` role that owns the control store.
- `create_project_database(name)` creates the project roles and database, and returns the `wamn_migration` URL.

The helper waits until a client connects.
Over 10 runs one after another, the median start time of PostgreSQL is 1.05 seconds.

The file store tests start their own MinIO container with `docker run`.
`crates/environment/tests/support/files.rs` pins its image. At the end of the test, it removes the container.

## Ignored tests

A test that needs a resource that `cargo test` does not supply is ignored.
Its marker names each need: `#[ignore = "requires: NAME, NAME"]`.
Cargo prints the reason with each ignored test.
A target sits in the tier of its strongest resource.

| Name | Need |
| --- | --- |
| `docker` | A running Docker daemon. The test starts its own containers. |
| `bin:psql` | `psql` on `PATH`. The test runs the client on the host. |
| `bin:helm` | `helm` on `PATH`. The test renders chart templates with `helm template`. |
| `bin:docker` | The `docker` command on `PATH`. |
| `cluster` | An installed platform on a kind cluster. |
| `env:NAME` | The environment variable `NAME`, set to the input that the test reads. |

To run one ignored test, give its exact name:

```bash
cargo test -p <package> --test <target> <test> -- --ignored --exact
```

For a test in a library target, use `--lib` in place of `--test <target>`.
An exact filter that matches no test still passes. Make sure that the result names one executed test.

### Generated exclusion test

The platform fixture test `data::error::tests::generated_update_exclusion_from_postgres` needs `env:WAMN_EXCLUSION_DIAGNOSTICS`.
Run it through its example:

```bash
cargo build --locked --offline -p xtask
cargo run --locked --offline -p wamn-test-infrastructure --example exclusion_diagnostics
```

The example copies the tracked application sources into a temporary directory.
It starts PostgreSQL, applies the platform fixture migration and adds an exclusion constraint on `widget`.
It captures the server diagnostics of a refused update, and generates the fixture code again in the copy.
Then it runs the ignored test with the diagnostics, through `cargo xtask require-test-result`.
The checkout does not change.

## Test helpers

Shared test helpers live in `wamn-test-infrastructure`, at `test-support/infrastructure/`.
It is a development dependency only.
A helper that one crate uses lives in the `tests/common/` directory of that crate.
For example, the deployment fixture of `application_routes_kind` lives in `tests/journeys/tests/common/`.
No crate in either workspace defines a `test-util` feature.
Tests use the production constructors with real inputs. Production code has no test-only constructor.

## Grant refusal tests

`wamn-grant-tests`, at `test-support/grant-tests/`, holds the grant refusal tests of every restricted deployed login.
Its targets are `project`, `system`, `credential_tables` and `transaction_identity`. Each test has the marker `requires: docker, bin:helm`.
The installers are `wamn_test_infrastructure::database::project()` and `system()`.

```bash
cargo test -p wamn-grant-tests -- --ignored
```

Each test starts its own PostgreSQL server.
It renders the database templates of the `project` or `platform` chart with `helm template`.
It applies the rendered Cluster and Database resources as CloudNativePG does.
Then it installs the product SQL through the product functions.

Each test logs in as the real login.
The project logins are `wamn_migration`, `wamn_app_login`, `wamn_query_login`, `wamn_platform_login` and `wamn_cdc`.
The system logins are `wamn_platform_sql_<digest>`, `wamn_identity_issuer_<digest>` and `wamn_pooler_auth`.
The org logins are `wamn_control_<digest>` and `wamn_apply_<digest>`.
CloudNativePG gives these logins a client certificate. The test gives each one a password, because a grant depends on the role and not on the login method.

The tests make sure of these facts for each login:

- The login has no superuser, role creation, database creation or row-security bypass attribute.
- The login is a member of no role with one of these attributes.
- Only `wamn_cdc` has the replication attribute.
- The login holds no grant option.
- The test derives from the catalog each action outside the effective privileges of the login.
- Each such action runs in a rolled-back transaction, and the server refuses it with SQLSTATE `42501`.
- `PUBLIC` executes no security definer function.

The `project` and `system` targets also hold the cross-org row policy tests, the extension tests and the catalog checks of REFERENCES and function EXECUTE.

`wamn_system_reader` is a group with no deployed login. Its test enters the group with `SET ROLE`.

`credential_tables` makes sure that `wamn_app_login` and `wamn_query_login` have no grant on `platform.credential`, `platform.oauth_token` or `platform.oauth_authorization`.
`transaction_identity` binds identities with `wamn_postgres::bind_transaction_identity`. A bound identity holds for its own transaction only.
The migration and platform logins resolve a seeded platform principal through `platform.login_principal`.

## What a test asserts

A test stays only on two conditions. A realistic defect fails it, and nothing else rejects that defect more strongly.
The expected value of the test does not come from the code under test.

- Tests check returned values, error types, parsed contract fields and database changes.
- No test compares diagnostics, source text, DDL text or file contents to a string.
- Tests do not check test helpers with fabricated failures.
- A byte or digest test stays only for a persisted or wire contract, with an independent expected value.
- A source or repository policy test is a lint in `cargo xtask repo-lint`, not a test.
- A schema rule is tested against the installed database: a refused write, a cascade or a `pg_catalog` fact.
- A new live test does not repeat an existing one. If a rule has no other test, extend the existing test.

### Decisions: retained text contracts

Exact text remains in the cases below, because the bytes or the literal are the public value under test.
The linked assertions hold the exact vectors. Do not copy those vectors into another fixture.
Do not use these exceptions for diagnostic wording.

- [Record history projection](../../apps/crates/record-history/tests/fold.rs), `retained_columns_keep_their_raw_spelling_and_order` and `retained_timestamptz_columns_take_one_spelling_from_either_spelling`: retained JSON tokens keep their spelling and order.
- [Catalog serialization](../../crates/schema/src/ir.rs), `catalog_without_views_keeps_canonical_bytes`: canonical persisted bytes define identity. [Durable JSON identity](../../crates/digest/src/lib.rs) keeps numeric spelling, signed zero and canonical key order.
- [SHA-256 vectors](../../crates/digest/src/lib.rs) and [HMAC vectors](../../crates/http/src/authentication.rs): independent encoded vectors define the digest or the signature.
- Frozen serialized variants, refusal codes, media types, SQLSTATEs, token prefixes and protocol identifiers remain literal comparisons. Their owners include [SQL values](../../crates/scalar/src/lib.rs) and [identity protocols](../../crates/identity-protocol/src).
- [HTML escaping](../../services/control/src/control_route/oauth_callback.rs): `&lt;script&gt;&quot;&amp;&#39;` encodes the supplied attack text. The unescaped text stays absent.
- [Manifest loading](../../services/edge/src/release_manifest.rs), `an_unsupported_format_refuses_with_the_frozen_literal`: the loader keeps the serialized refusal literal.
- [The log viewer](../../apps/platform_ui/test/environment.test.tsx) displays the supplied log bytes unchanged. The assertions read them from the recorded response.
- [Descriptor limit observation](../../services/host/tests/native_descriptor_limits.rs): `Max open files` identifies the Linux process limit record. The test parses its two numeric values while the real host runs.

Accessible control names select the action under test. They do not assert diagnostic wording, classes or geometry.
Secret-redaction checks search for the actual supplied secret. Payload round trips compare the original value of the caller.

## Local application business tests

Receiving and WMS keep their business assertions in `wamn-receiving-tests` and `wamn-wms-tests`.
The Receiving data-access tests have the marker `requires: docker` and start their own PostgreSQL server.
The WMS business scenarios need a product deployment fixture on a development cluster before they can run.
No in-process application assembler supplies them.

```bash
cargo test -p wamn-receiving-tests -- --ignored
```

The edge tests in `wamn-edge` read `WAMN_INGRESS_COMPONENT` and `WAMN_EDGE_DEVICE_COMPONENT` from `prepare-tests`.
Each test binary builds the release bundle of [`apps/edge_device`](../../apps/edge_device/README.md) once.
The `ingress` guest must match its pin in `apps/guests/ingress/ingress.wasm.sha256`.

### Local saved-edit acceptance

No automated test runs the saved-edit case.
The `wamn dev` tests in `services/cli/tests/dev_command.rs` check only that the command refuses a missing or unreadable environment document.
[Development loop](development-loop.md) gives the commands that build, publish and deploy one saved edit.

## Development on kind

The journey tests in `wamn-journey-tests` run on a local kind cluster through product commands.
Each test has the marker `requires: cluster, bin:helm`.
SQL only observes results, through a read-only login and a port-forward.

```bash
cargo xtask preflight
cargo xtask journey-cluster
cargo test -p wamn-journey-tests -- --ignored
```

`cargo xtask preflight` checks the charts before a platform deploy.
It renders the charts, and checks that their images resolve and that the wamn containers accept their arguments.
Then it sends every object to the kind API server in a server-side dry run.
Without `--chart <bundle>`, it builds a bundle under `target/preflight`.
With `--print-cluster-requests`, it prints the cluster requests and does not read the cluster.

`cargo xtask journey-cluster` creates the platform that the tests use.
It creates or reuses the `wamn-local` cluster and its registry on port 5001.
If the cluster CNI does not enforce NetworkPolicy, it fails.
It uses the bundle that `--chart <bundle>` names, or builds one under `target/journey/bundle`.
If the platform does not run that bundle yet, it runs `wamn platform deploy`.
It records container restarts, and fails only on a platform pod that never becomes ready.
Then it creates the read-only observer login on the system database.
It writes `target/journey/kubeconfig` and `target/journey/cluster.json`.
The tests read `cluster.json`. A missing file or entry fails the test.

Each test creates its own org with `wamn org create` and a unique owner email, and opens the owner session.
Then it applies its own environment with `wamn env apply`.
At most 4 tests hold an org at one time.
At the end of a test, a plain `wamn env delete` removes its environment, also after a failure.
The org stays.
One cluster holds up to about 12 orgs, because of the Pooler limits of the system database (wamn-44p7o).
A run that needs more orgs uses a second fresh cluster.

The `nats_outage` target stops the platform NATS server, so it must run alone.
Cargo runs the test targets one after another, so the command above runs it alone.
Do not run it beside another journey run.

### Platform and environment apply on kind

[Deployment](deployment.md#local-platform) gives the `wamn platform deploy` and `wamn env apply` procedure for a local cluster.
`cargo xtask kind-cluster-create` creates or reuses the `wamn-local` cluster and its registry on port 5001.

### Removed application cluster coverage

The Receiving and WMS cluster suites are removed, with their installers.
The RC runner and the separate identity and session cluster journey are also removed.
No test replaces their combinations of command histories, delivery, materialization and host restart.
Native application, identity, session and render tests remain at their own tiers.

## Capture a run

Ordinary tests need no result directory.
Report the command, the source revision, and the pass, fail or skip outcome.
Run output stays local and does not go into the checkout.
For a command that needs a result directory, create a temporary one:

```bash
WAMN_RESULTS="$(mktemp -d)"
```

Keep the source unchanged while a command runs.
Keep credentials in private files and out of shared output.
[Result interpretation](../testing/evidence.md) defines passes, failures and unexecuted cases.

### Acceptance runs

Owner ruling 2026-11-05: the non-cluster selection of an acceptance run always runs to completion.
It does not stop at the first failure. Every step runs.
Report every failure together in one report, with its log lines.
Do all the diagnosis that the failures need in the same pass, without further approval.
This diagnosis includes single-case reruns that keep their artifacts, and builds of the same case at the base commit.
The result is one run, one complete list of findings and one ruling session.
A cluster stage still stops at its first failure.

## Application generation and SQLx

`cargo xtask prepare-tests` builds every application for a test run.
To build selected packages, use `wamn build` ([building](building.md#wamn-build)):

```bash
cargo run --locked --offline -p wamn-cli --bin wamn -- build \
  apps/platform_fixture apps/platform_fixture_overlay
```

Give each base before its overlays.
The output goes to `apps/target/wamn/<package>`, and Git holds none of it.
The crate `wamn-fixture-package` states the platform fixture paths, so a test reads them without a literal of its own.
Do not edit generated Rust directly.

The compiler checks every emitted statement against the committed SQLx descriptions before it writes the bindings.
It records parameter types, selected columns, relation access and transaction needs in the compiled package document.

### wamn build

The live build tests in `wamn-postgres` have the marker `requires: docker`:

```bash
cargo test --locked --offline -p wamn-postgres --test build_live -- --ignored
```

Each test copies the platform fixture into its own directory and builds it against its own server.
The build creates and drops its own verification database on that server.

### Web packages

The web packages form one pnpm workspace, which `pnpm-workspace.yaml` at the repository root lists.
Use pnpm for every command of these packages, not npm.
Install all of them once, at the repository root:

```bash
pnpm install
```

The workspace has one lock file, `pnpm-lock.yaml`, at the repository root.
The platform UI scripts write their OpenAPI documents with `cargo xtask platform-ui-openapi` before they run.
So these scripts need the Rust toolchain, and the first run compiles xtask.

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

Browser sessions use the installed web, identity and control services.
Use the [development loop](development-loop.md) to publish and deploy an application before you open its screens.

### Shared Vite tooling and Receiving

`web/shell` keeps the shared Vite helper and the Playwright dependency for browser tests.
The platform UI owns the application shell and its tests.
To compile the helper and the Receiving web application, run:

```bash
pnpm --dir web/shell lint
pnpm --dir apps/wamn_receiving/web lint
```

## Cleanup

The test drops each PostgreSQL container at its end. Then testcontainers removes the container and its anonymous volumes.
A test process that is killed, for example with SIGKILL, does not remove its containers.
To find leftover test containers, run:

```bash
docker ps -a --filter label=org.testcontainers.managed-by=testcontainers
docker ps -a --filter name=wamn-files-
```

The second command finds the MinIO containers of the file store tests.
Remove only the containers that your run created, by their exact ID:

```bash
docker rm -f -v <container id>
```

Make sure that no other test run is active before you identify a container as yours.
Never use `docker prune`, broad image removal or name-substring cleanup.

For a cluster, find the clusters that your run created with `kind get clusters`.
Delete only those clusters:

```bash
cargo xtask kind-cluster-delete <cluster>
```

The command deletes the cluster. It fails on a container of that cluster that remains.
Manual platform cleanup keeps the shared build registry, as [deployment](deployment.md#cleanup) describes.
Never remove a cluster that another session created.
Use explicit cluster names, kubeconfig paths and contexts for Kubernetes and Helm commands.

Report unresolved cleanup failures.
Before you remove a worktree, keep its source changes and branch.
Remove only that worktree and its own target directory, after no process uses them.

## Benchmarks

`cargo xtask bench` runs benchmarks. The `bench-*` commands collect and summarize measurements. SQL replay inputs live in `xtask/bench/`.
[Transaction identity retention](deployment.md#transaction-identity-retention) gives `cargo xtask measure-tx-identity`.

## Host and package runtime

Run the lifecycle tests of the combined host:

```bash
cargo test --locked --offline -p wamn-host --bin wamn-host host::tests::
```

For authorization, component execution and database behavior, run the [local application business tests](#local-application-business-tests).
