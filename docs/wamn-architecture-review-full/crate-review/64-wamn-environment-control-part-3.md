# 64 wamn-environment-control, part 3

Scope: application SQL and how verbs execute, at `epic/dep7` (b94833fd): `environment/sql.rs` (772), `sql/schema.rs` (1,288), `sql/reset.rs` (947), `environment/operation.rs` (246). `platform_migrations.rs` is already ruled (tab 60 finding 5 B). Settles tab 60 finding 4 (schema lifecycle). Nothing built or run.

## Earlier work checked first

- Tab 60: one migration role (6 A3) on the SQLx `Migrator`; `--resolve`, `started` and `adopted` go; overlay check moves to build (3 B). Owner 2026-11-05: `schema.mode = reset | migrate`.
- Tab 62: Helm history is the applied-release record; SQL Jobs and credential Secrets are explicit steps (2 A); registry holds configuration (3 B). Tab 63: an application is its release (1 A); one checked plan (2 A).
- Tab 58 finding 6: one Kubernetes owner. Owner 2026-11-12: far too restrictive at apply and deploy.

## Files read

- `sql.rs`: function list, `Operation`, `run_deployed` (verb selection, Job construction, secrets).
- `sql/schema.rs`: types, `plan`, `requires_reset`, function list. `sql/reset.rs`: function list and the recreation path.
- `operation.rs` (in full, non-test).
- `wamn-package-control`: `apply_package.rs::installed_physical_fingerprint`, `package_version.rs` fingerprint functions, `schema_baseline.rs` head.
- `crates/execution/host/src/control_route/operation_job.rs` (structure). `services/ctl/src/application_verbs.rs`, `operation_verbs.rs` head.

## Callers read

- `run_deployed` (from `sql.rs`, `sql/reset.rs`); `platform::job::run` (from `sql.rs`, `platform/bootstrap.rs`, `platform/identity.rs`); fingerprint functions across crates (grep).

## One purpose per file

- `sql.rs`: run one of seven SQL operations (`provision`, `project-schema-diff`, `project-schema-apply`, `reset`, `initialize`, `schema-apply`, `schema-adopt`) as a Kubernetes Job of the `ctl` image with mounted credentials.
- `sql/schema.rs`: read schema state, plan a schema change against candidates, adopt, apply, status.
- `sql/reset.rs`: rebuild a disposable environment under its database name.
- `operation.rs`: the in-cluster executor of server-authorized operations (`deploy`, `rollback`, `diff`, `apply`, `reset`, `adopt`).

## Findings

**1. Every deploy verb has two execution paths.**

Fact: `wamn deploy` and `wamn rollback` (`application_verbs.rs`) call the control host route when `ControlHost::from_env()` finds one. The host's `operation_job.rs` starts a Job of the `ctl` image that runs `environment::operation::run`, which calls the same `deploy_as`, `apply_selection` and `schema` functions. Without a control host, the CLI runs those functions locally with a kubeconfig and database URLs, and `sql::run_deployed` starts its own SQL Jobs for seven `env` subcommands. The two paths differ in who is authorized: the server checks the caller (`WAMN_DEPLOY_ACTOR`); the local path trusts whoever holds the kubeconfig and database URLs.

Options:

- A: one path, the control route. Clients call the route; the operation Job is the only executor. The seven SQL `env` subcommands become internal steps of that executor, not public verbs. A kubeconfig path stays only for platform bootstrap (before a control host exists) and dev.
- B: one path, the local CLI with kubeconfig. The server operation route, `operation_job.rs` and `operation.rs` go.

**2. Two Job runners.**

Fact: `platform/job.rs` (580 lines: manifest, mounts, secrets, wait, logs, cleanup) runs SQL, bootstrap and identity Jobs. `crates/execution/host/src/control_route/operation_job.rs` (572 lines) builds and watches operation Jobs separately.

Options:

- A: one Job runner in `wamn-kubernetes` (tab 58 finding 6) used by both.

**3. Schema state is identified by fingerprints beside migration history.**

Fact: `sql/schema.rs::plan` compares a physical schema fingerprint per package (`installed_physical_fingerprint`: manifest models plus live columns), an environment fingerprint (`environment_fingerprint`, `installed_environment_fingerprint`), the initialization identity and the database instance. `Diff::requires_reset` is true in reset mode when the candidate environment fingerprint differs from the installed one. Fingerprints appear in 15 files across environment, package, schema-control, generator and dev; `schema_baseline.rs` and its plan are 517 lines. The plan also lists "unknowns" such as "live schema and executable compatibility are not inspected".

Options:

- A: migrate mode is `Migrator::run`, which refuses a changed or missing applied file by checksum. Reset mode resets on every schema apply. `schema diff` reports the `Migrator`'s pending and mismatched files. Fingerprints, `requires_reset`, the environment fingerprint, the initialization identity check and `schema_baseline` go.
- B: reset mode resets only when the `Migrator` reports a changed or missing applied file. Fingerprints go as in A.

**4. Reset rebuilds the database through CNPG.**

Fact: `reset_deployed` stops the environment's hosts (Helm values `stop_values`), recreates the CNPG `Database` (`recreate`), initializes it, reconnects the CDC capture gap (`reconnect_capture_gap`), and resumes hosts. `registry.project_envs.pending_database` records the inputs so an interrupted reset can resume. It needs the recorded sources of every application in the environment (`reset_sources`, `missing_reset_sources`).

Options:

- A: under one migration role (tab 60 A3), reset stops hosts, runs `DROP OWNED BY <migration role>` (every application object), clears application rows in `catalog`, reruns every package's migrations, and resumes hosts. The database, its CNPG `Database`, credentials and CDC objects stay. `recreate`, `pending_database` and the recorded reset inputs go. Inference: the CDC publication loses dropped tables and must be refreshed after the rerun; checked in part 7 (CDC).
- B: keep recreating the database.

## Follows from earlier rulings

- Tab 60 6 A3: `schema-adopt` and `adopt_package_schemas` become `Migrator::skip`; `MigrationResolution` and `--resolve` leave `Operation`, `operation.rs` and `ctl`. Tab 60 finding 4 is settled by findings 3 and 4 here.
- Tab 62 1 A: `operation.rs` reads `/operation-inputs/configuration.json` and then overwrites `Platform` fields; it reads the installed platform instead. Tab 62 3 B: `operation.rs` reads the registry, not the ConfigMap. Tab 62 4 A: its `"hosts"` literals become constants.
- Tab 63 1 A: `application_refusals` calls in `sql.rs`, `sql/schema.rs` and `sql/reset.rs` go.
- Tab 60 5 B: `project-schema-diff` and `project-schema-apply` run the platform project scope on the `Migrator`.

## Tests deleted, changed or moved

- Finding 1 A: tests of the local deploy path and of the public SQL `env` subcommands; finding 1 B: `operation_job.rs` tests and `operation.rs` tests.
- Finding 3: fingerprint tests in `package_version.rs`, `package_artifact.rs::physical_fingerprint_survives_publication_and_nonphysical_edits`, `schema_baseline` tests, `sql/schema.rs` plan tests.
- Finding 4 A: reset recreation tests in `sql/reset.rs` and `services/ctl/tests/reset_lock_live.rs`.

## Not read

- `sql/schema.rs` bodies of `adopt_application`, `apply`, `status`; `sql/reset.rs` bodies other than the function list and recreation.

## Rulings

- 2026-11-12: all findings A. 1 A: the control route is the one execution path; kubeconfig only for platform bootstrap and dev. 2 A: one Job runner in `wamn-kubernetes`. 3 A: `Migrator` checksums in migrate mode, reset on every schema apply in reset mode; fingerprints and `schema_baseline` go. 4 A: reset by `DROP OWNED BY` the migration role and rerun; CDC publication refresh checked in part 7. Tab 60 finding 4 is settled.
