# 60 wamn-package-control

Scope: `crates/control/package/src` (11,236 lines, 38 files; tests 718). Structure, callers, and the files named below. Nothing built or run.

Rechecked at `epic/dep7` (b94833fd) on 2026-11-12. Findings 1–4 still stand. Dep7 adds hand-written migration history: findings 5 and 6.

## Earlier work checked first

- Tab 42: no freshness checks. Tab 51 finding 4: the manifest parse validates; separate `validate_operation_vocabulary` calls go. Tab 53 finding 4: contract files deleted; readers take a typed view of `wamn.json`.
- Tab 12 finding 2 A: the runtime's SQL text lists were deleted because admission already refuses the same constructs on the parsed SQL (`declared_sql.rs`). Tab 11 finding 19: the `set_config` and `wamn.` message checks stay.
- Tab 50: all SQL reading uses `pg_query`; `wamn-schema-introspection::migration_policy` scanner deleted. Tab 44: `tools/guest-rustflags` becomes a Rust function. Tab 40: one build path for components.
- Owner 2026-11-10: no hand parsing of SQL; reads and writes are governed by roles. Owner 2026-11-05: per-environment `schema.mode = reset | migrate`.
- U2 (SQLx), tab 06 (`wamn-pg-core` deleted), tab 01 (`wamn-execution-contract` split), `hyper` over `reqwest`.

## Files read

- `lib.rs`, `Cargo.toml`; headers of every module; `package_artifact.rs` header; `component_admission/declared_sql.rs` (in full); `integration.rs` (build); `apply_package/definition_ownership.rs` and `plan.rs` (structure, SQL); `apply_package/adoption.rs` (head); `apply_package/roles.rs`.
- OCI client construction across the workspace (grep); callers of `admit_component`, `adopt_package_schemas`, the apply and fingerprint functions (grep).

## One purpose per file

- `package_artifact.rs`: `push-package` and reading a pushed package.
- `component_admission.rs` (+ `plan.rs`, `declared_sql.rs`): admit component bytes against package, declaration and declared SQL; publish one OCI layer.
- `component_declaration.rs`, `connection_requirement.rs`: component declarations and connection requirements.
- `integration.rs`: build and publish integration wasm.
- `web_artifact.rs`, `web_scope.rs`: web artifacts, bucket upload, `config.json`.
- `apply_package/*`: install or advance package SQL; definition ownership; record history; entity maps; adoption.
- `schema_baseline.rs`, `database.rs`, `sql_params.rs`: schema digests and `tokio-postgres` adapters.

## Findings

**1. The OCI client is set up seven times in five crates.**

Fact: `OciClient::try_from(ClientConfig { .. })` in `wamn-package-artifact` (`lib.rs`, `registry_pull.rs` twice), `wamn-runtime` (`component_artifact_source.rs`, `release_manifest_source.rs`), `control/release/publish_release.rs`, and here (`component_admission.rs::registry_client`).

Options:

- A: one OCI client constructor (protocol, roots, auth) in `wamn-package-artifact`; the others call it.

**2. A second build of wasm in the package crate.**

Fact: `integration.rs` runs `tools/guest-rustflags` and `cargo build --target wasm32-wasip2` as subprocesses and reads Cargo's JSON messages, beside the component build in the generator (tab 40).

Options:

- A: integration components build through the one component build path (tab 40), with the rustflags function of tab 44. The subprocess code here goes.

**3. Definition ownership is tracked per column and constraint in a catalog table.**

Fact: `catalog.package_definition_owners` records which package owns each relation, column and constraint (with `client_field_extensible` for overlays). Before apply, `validate_definition_ownership_before_apply` and the `preflight_*` functions parse migrations (`inspect_migration_definition_mutations`, the introspection scanner tab 50 deletes) and refuse a change to another package's definition. Migrations then run as the package owner role (`SET LOCAL ROLE`), so PostgreSQL refuses a change to a relation the role does not own. Not read: how an overlay package adds a column to a relation it does not own.

Options:

- A: keep per-definition ownership (needed for overlays at column grain); its migration reading moves to `pg_query` (tab 50).
- B: delete the ownership table and the preflight; PostgreSQL relation ownership is the only check. Overlay columns are then added by the owning package's role, not by the overlay package.

**4. Schema lifecycle functions used by environment and dev.**

Fact: `apply_package_schema`, `apply_local_package(_schema)`, `adopt_package_schemas` (record schemas as installed without running SQL), `installed_environment_fingerprint`, `environment_fingerprint`, `schema_baseline` are called by `control/environment/.../sql/schema.rs` and `control/dev/.../target_database.rs`.

Options:

- Decide with the control environment tab, against `schema.mode = reset | migrate` (owner 2026-11-05).

**5. The platform migration scopes have no migrations (new at dep7).**

Fact:

- `deploy/sql/migrations/system` and `deploy/sql/migrations/project` hold only `.gitkeep`.
- Code that serves them: `wamn-control-provision` `build.rs` (50 lines, embeds the two folders) and `schema_migrations.rs` (29); `deploy/sql/schema-migration-history.sql` (37, a `schema_migration` table with an immutability trigger); `control/environment/platform_migrations.rs` (480); the platform history part of `schema_baseline.rs` and `schema_baseline/plan.rs` (both grew at dep7).
- Callers: `environment/sql/reset.rs`, `environment/ensure.rs`, `provision_system.rs`, `reconcile_run_plane.rs`, `dev/target_database.rs`, `ctl` `platform_verbs.rs` and `env_verbs.rs`.
- Repo rule (CLAUDE.md, standing rule 1): greenfield; refuse old rows, bump formats, no compatibility paths.

Options:

- A: delete the platform scopes now. Add them back with the first platform migration, on the SQLx `Migrator` (finding 6). Same approach as the owner's ruling on release qualification (2026-11-10).
- B: keep them, moved onto the SQLx `Migrator` (finding 6 A).

**6. Package migrations run on a hand-written runner, not the SQLx `Migrator`.**

Fact (this repo):

- `wamn-schema-control::migrations` (278): `NNNN_snake.sql` names, order, gap and duplicate refusal, SHA-256 drift check, `validate_execution_sql`.
- `wamn-package-control::migrations` (361): `execute_migrations` with `MigrationHooks`; a `started` record before each no-transaction file; `--resolve <package>:<path>=applied|retry`; `lock_session` (a try-lock loop; its comment: a blocking lock can wait forever with `CREATE INDEX CONCURRENTLY`).
- `apply_package.rs` writes `catalog.package_migrations`, one table keyed by `env_key` and package. Outcomes: `applied`, `adopted`, `started`.
- Package hooks per file: before, the definition ownership check (finding 3); the file runs as the package role; after, ownership reconcile.
- These crates use `tokio-postgres`; U2 moves them to SQLx.

Fact (SQLx 0.9 `Migrator`, docs.rs source):

- Embeds files, checksums them, runs them in order, refuses a changed applied file (`VersionMismatch`) and a missing one (`VersionMissing`). Supports no-transaction files, `dangerous_set_table_name`, `create_schema`, `set_locking`.
- Its lock is a blocking `pg_advisory_lock`, one per database.
- It writes the record only after a file succeeds. A crash inside a no-transaction file reruns that file next time.
- One history table per `Migrator`; no key column; no per-file hooks.
- Not checked: whether it refuses a gap in version numbers.

Checks (2026-11-12, code and SQLx 0.9.0 source read; nothing built or run):

1. Gaps and order. Fact: `Migrator` refuses a changed applied file (`VersionMismatch`), a missing applied file (`VersionMissing`) and a dirty row. It does not refuse a gap, and it applies a new file numbered below the latest applied one. Today `migrations.rs::normalized_migrations` and `validate_recorded_prefix` refuse both.
2. `skip`. Fact: `Migrator::skip` records pending files as applied without running them. It can stand in for `adopt_package_schemas`, which today adopts several packages in one transaction. Inference: atomic adoption of several packages needs the runs on one outer transaction (SQLx then uses savepoints).
3. Lock. Fact: with `set_locking(false)`, `run_direct` calls neither `lock` nor `unlock`. Our `lock_session` loop can replace it.
4. Roles: the main blocker. Fact: `roles::execute_migration` parses each file with `pg_query` and runs it statement by statement. Each statement runs as the owner of its target relation (`physical_schema::migration_owner`), so an overlay's `ALTER TABLE` on a base relation runs as the base package's role. Before each statement it grants `REFERENCES` on foreign-key targets (and `USAGE` on `platform`) that the role lacks, and revokes them after. `Migrator` runs a file's whole text as one `execute` under one session role; it cannot switch roles or grant per statement.
5. Caller binding. Fact: `wamn_history.stamp_row` reads `current_setting('app.user_id', true)`; `bind_platform_principal_sql` and `CLAIM_ENV_KEY_SQL` use `set_config(..., true)` (transaction-local). Inference: session-level `set_config(..., false)` on a dedicated connection works for the triggers. The connection must not return to a pool afterwards.
6. `before` hook. Fact: `validate_definition_ownership_before_apply` reads `catalog.package_definition_owners`; its comment says later files read the owners recorded by earlier files. So it depends on the `after` hook of the earlier file. Inference: one check over the mutations of all pending files, in order, gives the same result for create and add; alter and drop are refused before execution (`reconcile_definition_ownership` marks them unreachable).
7. `after` hook. Fact: `reconcile_definition_ownership` checks each definition exists in `pg_catalog` and inserts owner rows with `ON CONFLICT DO NOTHING`. It is idempotent. Inference: run once after `Migrator`, over all files, so a crash between the run and the reconcile is repaired by the next apply.
8. `finish` hook. Fact: `execute_migrations` commits each file, then runs `finish` in its own transaction. Nothing in `finish` commits with the last file today. Moving it after `Migrator` loses nothing.
9. Readers of `catalog.package_migrations`: `schema/control/sql.rs::select_package_migrations_sql`, `environment/sql/schema.rs` (packages union migrations), `package_version.rs` (`started` or orphan rows), `adoption.rs` (insert `adopted`), `run_plane/declarations.rs` and `observation_sql.rs` (grant checks). The per-file `sha256` is read only for the drift check, which `Migrator`'s checksum replaces.
10. No-transaction files. Fact: a file is non-transactional when its first line is `-- wamn:no-transaction` (`manifest.rs::migration_is_transactional`); such a file must hold one statement. No `.sql` file in the repository has that line. With `Migrator::with_migrations` we set `no_tx` per file ourselves.
11. Names. Fact: org, project and env names are up to 63 bytes each, so `env_key` can pass 190. `package_owner_role` already hashes the package id into a fixed-length name. Inference: the history table name can follow it (prefix plus a 40-hex hash of `env_key` and package).
12. Privilege on the history table (inference, not checked): PostgreSQL may check `CREATE` on the schema before `CREATE TABLE IF NOT EXISTS` sees the table exists. If so, the session role needs `CREATE` on the history table's schema.
13. History stamps and immutability (correction to the first version of this finding): `Migrator` creates its table with `IF NOT EXISTS` and inserts only its own columns. A table we create first can carry `created_by`, `updated_by`, the `stamp_row` trigger and an immutability trigger. They are not lost.

Options:

- A1: `Migrator`, with one role per run. Each overlay package role is granted membership in the base package roles it extends (PostgreSQL lets a member alter a relation its role owns), and package roles hold standing `REFERENCES` on the platform tables they may reference (owner ruling 2026-11-10: platform references are proper foreign keys). The per-statement role switch and temporary grants go. Keep a small gap and order check before the run, the `lock_session` loop, a whole-run ownership check before and reconcile after, session-level caller binding, and a pre-created history table per package and environment. `--resolve`, `started` and `adopted` go. Inference: about 600 lines go, not 800; the ownership code stays (finding 3 A).
- A2: `Migrator` with finding 3 B (PostgreSQL relation ownership only; an overlay does not alter base relations). Same as A1 without the overlay membership grants; the ownership table, its check and reconcile also go.
- B: keep the hand-written runner, which supports per-statement roles and per-file checks. Merge `wamn-schema-control::migrations` and `wamn-package-control::migrations` into one module on SQLx connections (U2).

* A3 (added 2026-11-12, owner question): one migration role per project database. A NOLOGIN role (for example `wamn_app_owner`), separate from the platform owner `wamn_db_owner`, owns every application schema and object. The apply runs `SET ROLE` to it and `Migrator::run`. Overlays alter base tables because one role owns both. The role holds standing `REFERENCES` on `platform.file(id)` and `platform.external_system(id)`. The rest is as A1: order check, `lock_session`, whole-run ownership check and reconcile, session-level caller binding, a history table per package and environment, `finish`. Beyond A1, these go: `package_owner_role`, `ensure_package_owner_role_sql`, the package-role grants in `environment/ensure.rs`, per-package schema owners in `ensure_model_schemas`, `set_package_owner_role` in `record_history.rs`, the package-owner exception in introspection `postgres.rs`, and all of `roles::execute_migration` except running the file. Lost: PostgreSQL no longer refuses one package changing another's objects; the migration policy and the ownership table are the only check (owner ruling 2026-11-10: strict isolation between applications means separate projects).

Checks for A3 (2026-11-12):

- Owner-dependent objects. Fact: `SECURITY DEFINER` appears only in platform SQL (`deploy/sql/app-schema.sql`, `system-schema.sql`, `control-portable-store.sql`), owned by platform roles. No package code or generated SQL creates `SECURITY DEFINER` functions, RLS policies or `GRANT ... TO` a package role; `CREATE POLICY` appears only in `run_plane/plan.rs` for platform tables. The per-relation history functions (`physical_schema/history.rs`) are created under the package role today and are invoker functions; under A3 the single role owns them.
- Escape routes in migrations. Fact: `wamn-schema-introspection::migration_policy` is an allowlist. Anything not admitted is refused (`UnsupportedStatement`). It refuses `DO`, `PREPARE`/`EXECUTE` (not admitted), `CREATE`/`ALTER`/`DROP` of function, procedure, trigger, rule, policy, language, type, domain, extension, event trigger, foreign table and materialized view, row level security changes, `SET ROLE` and `SET SESSION AUTHORIZATION`, `GRANT`, `REVOKE`, `ALTER DEFAULT PRIVILEGES`, role operations, `DROP OWNED`, `REASSIGN OWNED`, and qualified DDL outside the package's configured schemas. Data statements must target a table the package created. `CREATE INDEX CONCURRENTLY` is admitted only alone in a `-- wamn:no-transaction` file, on a table the package created.
- Gap found. Fact: the ownership table records `Relation`, `Field` and `Constraint` from `CREATE`/`ALTER`/`DROP TABLE` only. Views are not recorded. The policy admits `CREATE OR REPLACE VIEW` and `DROP VIEW` in configured schemas, which for an overlay include its bases' schemas. Today PostgreSQL ownership stops an overlay replacing or dropping a base package's view; under A3 nothing does.
- Note: the policy is a hand-written token scanner (tab 50 rules its move to `pg_query`). Under A3 it becomes the only check, so that move matters more.

## Follows from earlier rulings

- Tab 42: delete `check_current` (5 places), `read_current` (2), and the `push-package` refusal "run `wamn build`".
- Tab 53 finding 4: `GeneratedOperationContract` and the `generated/contracts/...` path in `component_admission/plan.rs` become the typed manifest view.
- Tab 51 finding 4: three `validate_operation_vocabulary` calls go.
- U2: `database.rs` and `sql_params.rs` (`tokio-postgres` adapters) go for SQLx.
- Tab 06: `wamn_pg_core::quote_ident` becomes the client library's. Tab 01: `wamn-execution-contract` uses become `wamn-digest`. Tab 02: `wamn-registry`.
- `hyper` over `reqwest`: the `reqwest::header` media-type constants become `http::HeaderValue`.
- Tab 59 finding 3: `ensure_package_owner_role_sql` and the other provision SQL become provision functions called with a connection.

## Fine as is

- `declared_sql.rs`: one parse with `pg_query`, the one owner of what a declared statement may do (tab 12 finding 2 relies on it). Its `platform.*` write check repeats what `wamn_app` grants already refuse; the counter, `set_config`, `wamn.` message and `DO`/`EXECUTE` checks have no grant equivalent.

## Tests deleted, changed or moved

- Tab 42 follows: `package_artifact_live.rs::push_package_refuses_stale_output_before_reading_its_manifest` and the stale-output unit tests go.
- Finding 3 B: definition ownership tests go.
- Finding 5 A: the platform migration tests in `control/environment/tests/provision_system_live.rs` (the `MigrationScope` cases) go.
- Finding 6 A: the tests of resolve, `started` and the gap rule go, including the `started` cases in `apply_package/package_version/registration_tests.rs`.

## Not read

- `package_artifact.rs` and `web_artifact.rs` bodies; `component_admission.rs` body beyond the named items; `apply_package.rs` body; `record_history.rs`, `entity_maps.rs`; tests.
- New at dep7: `web_artifact/platform_ui.rs` (393, publishes the shared platform UI); `schema_baseline.rs` and `schema_baseline/plan.rs` in full.

## Rulings

- 2026-11-12: findings 1 and 2 A. Finding 3: A for now (keep per-definition ownership, its migration reading on `pg_query`); option B (PostgreSQL relation ownership only, overlay columns added by the owning package) noted for later. Finding 4 carried to the control environment tab.
- 2026-11-12: finding 6: A3, one migration role per project database, on the SQLx `Migrator`. Open: the view gap (views are not in the ownership table). Finding 5 not yet ruled.
- 2026-11-12: view gap accepted. Owner: "a package is a package is a package"; packages composed together are not separate ownership. An overlay should ideally be additive only, but that is a dev check or verification, not an apply-time restriction.
- 2026-11-12: finding 3 changed to B. The additive-only overlay check moves to `wamn build` and dev (`pg_query` over the overlay's migrations against its bases). At apply, `catalog.package_definition_owners`, `apply_package/definition_ownership.rs` and `definition_ownership/plan.rs` (913 lines), and A3's whole-run check and reconcile go. Owner direction: "in general we are being way too restrictive at apply/deploy." Inference, not checked: the build has the bases' migrations through `base_dependencies`.
- 2026-11-12: finding 5 B. The platform scopes (system, project) move to the SQLx `Migrator` now: `sqlx::migrate!` embeds `deploy/sql/migrations/{system,project}`, each runs as the platform owner role with its own history table, called from today's call sites. The hand-written code goes: `platform_migrations.rs` (480), provision `build.rs` (50) and `schema_migrations.rs` (29), `schema-migration-history.sql` (37), and the platform history parts of `schema_baseline`. Tab 60 is fully ruled.
