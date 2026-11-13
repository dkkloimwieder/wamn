# 77 wamn-schema-control, wamn-schema-introspection, wamn-package-artifact

Scope at `epic/dep7` (b94833fd): `crates/schema/control` (5,742 lines with tests), `crates/schema/introspection` (`postgres.rs` 2,514, `ir.rs` 991, `migration_policy.rs` 1,689), `crates/catalog/package-artifact` (1,473). Includes the migration allowlist decision carried from tab 60. Nothing built or run.

## Earlier work checked first

- Tab 60: one migration role per project database on the SQLx `Migrator` (6 A3); platform scopes on `sqlx::migrate!` (5 B); the overlay additive check moves to build and dev, and the definition ownership table goes (3 B); the allowlist decision was carried here.
- Tab 62 5 A: the run-plane converge engine goes (`reconcile_run_plane.rs`, `wamn-schema-control::run_plane`). Tab 62 7 A: `claim_environment_instance` goes.
- Tab 64 3 A: fingerprints go. Tab 68: `reconcile_replica_identity` moves to the events and CDC owner.
- Tab 07 and tab 11: application dynamic SQL runs as a login that holds SELECT only; grants refuse writes.
- Tab 01: one digest function in `wamn-digest`. Owner 2026-11-11: hyper, not reqwest. Tab 19: the `pod_identity.rs` reqwest option was deferred to the runtime root files tab.
- Owner 2026-11-12: developer checks belong in build or dev, not apply or deploy.

## Files read

- `schema/control`: `lib.rs`, `connections.rs`, `sql.rs`, `model.rs`, `exposure.rs` in full; headers and item lists of every other file.
- `schema/introspection`: headers of `postgres.rs`, `ir.rs`, `migration_policy.rs`.
- `package-artifact`: `Cargo.toml`, `lib.rs` header and item list, `registry_credentials.rs` item list.
- PostgreSQL 18 docs, "Overview of Event Trigger Behavior".

## Callers read

- Repository grep for every public item of `wamn-schema-control`, for `wamn_schema_introspection::`, for the GKE metadata host, for `ring::`.
- `crates/client/core/src/route.rs` header; `crates/schema/generator/src/client_ir.rs::route_index` doc.

## One purpose per file

- `schema/control`: package migration planning (`migrations.rs`, `package_migrations.rs`, `sql.rs`), run-plane convergence (`run_plane/*`), environment instance claim, replica identity, physical schema rendering with history triggers (`physical_schema*`), route normalization (`exposure.rs`), shared statement values (`model.rs`), a digest helper (`connections.rs`).
- `schema/introspection`: read `pg_catalog` into the schema IR (`postgres.rs`, `ir.rs`); the migration allowlist (`migration_policy.rs`).
- `package-artifact`: pull and verify package artifacts and their component descriptors; registry credentials, trust roots and retries.

## Findings

**1. Earlier rulings leave `wamn-schema-control` almost empty.**

Fact, by file: `migrations.rs`, `package_migrations.rs`, `sql.rs`, `model.rs` serve the hand-written migration runner (tab 60 6 A3); `run_plane/*` (about 3,500 lines with tests) is the converge engine (tab 62 5 A); `environment.rs` is the instance claim (tab 62 7 A); `replica_identity.rs` moves to the CDC owner (tab 68); `physical_schema.rs::migration_owner` and `ExecutionOwner` serve per-model owners (tab 60 6 A3); `connections.rs::prefixed_sha256` is a digest copy (tab 01). What stays: the history trigger rendering in `physical_schema.rs` and `physical_schema/history.rs`, whose one production caller is `crates/schema/generator/src/build.rs`, and `exposure.rs` (finding 2).

Options:

- A: the crate stops existing. History rendering moves into the generator.

Recommend A.

**2. Route normalization lives in the wrong crate, and two readers work around a cycle that does not exist.**

Fact:

- `exposure.rs::normalize_http_route` and `canonical_http_route_template` run at release publish (`publish_release.rs`, `publish_release/attachments/plan.rs`), after the build has written the routes.
- `client_ir.rs::route_index` says `wamn-schema-control` "DEPENDS on this crate" (the generator); `client/core/src/route.rs` says it "depends on the generator". Both refuse an un-normalized route instead of normalizing it.
- `wamn-schema-control` depends on `wamn-catalog`, `wamn-record-history`, `wamn-pg-core`, `pg_query`, `serde`, `serde_json` and `sha2` (its `Cargo.toml`); the generator depends on it, not the other way.
- `wamn-catalog::route_identity.rs` already owns how an operation names its route path.

Options:

- A: the two functions move to `route_identity.rs`. The build normalizes once, when it validates `wamn.json`; publish reads routes as built. The two false comments are fixed.

Recommend A.

**3. The migration allowlist (carried from tab 60).**

Fact:

- `migration_policy.rs` (1,689 lines) admits a closed set of statements and refuses DO, functions, triggers, policies, row security, `SET ROLE`, `GRANT` and role commands; data statements may touch only tables the package created.
- Its production callers are `definition_ownership.rs` (goes, tab 60 3 B) and `platform_migrations.rs` (goes, tab 60 5 B). After those rulings it has none.
- PostgreSQL lets an object's owner grant privileges on it to any role. The migration role owns application objects (tab 68), so migration SQL could grant writes to the SELECT-only login of tabs 07 and 11.
- PostgreSQL 18 docs: `GRANT` and `REVOKE` fire `ddl_command_start` and `ddl_command_end`; commands on shared objects (roles and role memberships, databases, tablespaces) do not. The docs do not name `ALTER DEFAULT PRIVILEGES`.
- Inference: publication runs the build on the client (`dev/source_deploy.rs::publish_from_env`), so a check that runs only in the build can be skipped by a hand-made artifact.

Options:

- A: delete the allowlist. Database privileges are the only boundary; migration SQL may widen grants on application objects.
- B: delete the allowlist. Project initialization creates one event trigger that refuses `GRANT` and `REVOKE` when the current user is the migration role. Role commands are refused by the role's own attributes (no `CREATEROLE`, no role admin; inference from tab 65 2 B). Dynamic SQL inside DO or functions is caught too, because the trigger fires on the command itself. One check: whether `ALTER DEFAULT PRIVILEGES` fires the trigger.
- C: replace the allowlist with a short denylist parsed with `pg_query` at apply. Dynamic SQL in DO or functions escapes it.

Recommend B: the database enforces the one rule that guards other roles, everything else is allowed, and nothing is checked twice.

**4. Two GKE metadata token readers.**

Fact: `package-artifact/src/registry_credentials.rs::read_metadata_registry_credentials` and `runtime/src/pod_identity.rs` each fetch a token from `metadata.google.internal` with their own reqwest client.

Options:

- A: one reader, on hyper, used by both registry credentials and storage. Its home is decided with the runtime root files tab (tab 19).

Recommend A.

## Follows from earlier rulings

- Tab 01: `package-artifact::sha256_digest` (on `ring`) and `schema/control::prefixed_sha256` become the one `wamn-digest` function; the `ring` dependency goes.
- Tab 71 1 A: the `push-package` messages in `package-artifact` ("run push-package") name a verb that goes; publication is `source_deploy`.
- Owner 2026-11-11: `registry_credentials.rs` and `registry_pull.rs` move from reqwest to hyper.

## Fine as is

- `wamn-schema-introspection::postgres` and `ir`: the build reads the migrated schema from a disposable database; one reader.
- `wamn-package-artifact`: digest-checked pulls, descriptor reads, bounded retries.

## Tests deleted, changed or moved

- Finding 1: `run_plane/tests.rs` (1,255) and the package migration planning tests go; history rendering tests move to the generator.
- Finding 2: `exposure.rs` tests move to `route_identity.rs`; the refusal tests in `client_ir.rs` and `client/core/route.rs` for un-normalized routes go.
- Finding 3 B: `tests/migration_policy.rs` and the policy cases in `apply_package_live.rs` and `application_activation_live.rs` go; one live test that the event trigger refuses a `GRANT` from the migration role.

## Not read

- `migration_policy.rs`, `postgres.rs`, `ir.rs` bodies; `registry_pull.rs`, `integration.rs` bodies.

## Rulings

- 2026-11-12: 1 A (wamn-schema-control stops existing), 2 A (route normalization in route\_identity, once at build), 3 B (allowlist deleted; event trigger refuses GRANT and REVOKE by the migration role; ALTER DEFAULT PRIVILEGES to check), 4 A (one GKE metadata reader on hyper).
