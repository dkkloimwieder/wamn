# Schema upgrade

Updated through: 2026-09-30, `main` at `47dc5a3e7`. Finding `wamn-o8b9`. The owner reviews this spec before any code. It comes before the B issues of `wamn-ld93`.

## 1. Goal

One verb applies a change of the platform schema to an installed database: the system database `wamn_system`, and the platform schemas of a project-environment database. After it, no schema change reaches a deployed database by a hand statement or by a new provision.

The change adds one kind of file, a numbered platform migration, and one record table in each database. It uses the prefix rule that `apply-package` applies to package migrations today.

Package migrations of an application stay the subject of `docs/plan/upgrades.md`. This spec does not change them.

## 2. Fixed rules

- A platform schema change is a numbered migration file under `deploy/sql/migrations/system/` or `deploy/sql/migrations/project/`. The name is `NNNN_<name>.sql`, as for package migrations.
- The full schema files stay the fresh install. A change goes into the full file and into a new migration file in the same commit.
- Each database records the migrations it holds: path, ordinal and sha256. The recorded list must be a byte-identical prefix of the files, as `validate_recorded_prefix` requires for packages (`crates/schema/control/src/package_migrations.rs:622-680`). An edited applied file refuses the run.
- A fresh install records every migration file as applied, because the full files already hold the change.
- One run applies the pending suffix in one transaction and records it in the same transaction. A failure leaves the database as it was.
- A second run with nothing pending changes nothing and says so.
- The verb prints each file with its result on one line. It prints no credential.
- Without `--confirm`, the verb prints the pending files and changes nothing.

## 3. Current state

Measured on `main` at `47dc5a3e7` on 2026-09-30.

| Place | Today |
| --- | --- |
| System install | `provision-system` runs `record-history.sql`, `system-schema.sql` and the control portable store as `wamn_system`, with no explicit transaction (`crates/control/lib/src/provision_system.rs:52-100`). A second run refuses: "the system database already has the schema registry; provision-system runs once" (`:61-64`). |
| System version | `registry.meta.schema_version` is `'0.1'` from the install, and nothing updates it (`deploy/sql/system-schema.sql:114-123`). No table records applied platform SQL. |
| Idempotency | `system-schema.sql` has 20 plain `CREATE TABLE` and no `IF NOT EXISTS`. `control-portable-store.sql` has 12. `record-history.sql` is idempotent: `CREATE SCHEMA IF NOT EXISTS` and `CREATE OR REPLACE FUNCTION`. |
| Project install | `reconcile-run-plane` needs SUPERUSER or BYPASSRLS and uses no `SET ROLE` (`crates/control/lib/src/reconcile_run_plane.rs:796-810`). If `catalog` is absent, it installs the catalog schema with `record-history.sql` inside it (`crates/schema/control/src/run_plane/plan.rs:494-499`). If `app_system` is absent, it installs `app-schema.sql` (`reconcile_run_plane.rs:527, 625-632`). Otherwise it installs neither. It diff-plans `run-state.sql` and `run-queue.sql`: missing tables, added columns, indexes, checks, helper functions and named cutovers (`reconcile_run_plane.rs:14-25`). |
| History tables | `apply-package` creates each model's history table with `wamn_history.create_history_table` as `wamn_db_owner`, and skips a table that exists (`crates/control/lib/src/apply_package/record_history.rs:35-65`). `app-schema.sql` creates the `app_system` history tables. |
| Package migrations | `catalog.package_migrations` records path, ordinal and sha256. An edited applied file refuses with `package-migration-drift`. |
| Releases | `publish-release` refuses a package that is not applied (`crates/control/lib/src/publish_release.rs:1545-1548`). `select-release` sets the head. `deploy-release` refuses a schema-changing package on an installed database: "schema-changing deployment requires a fresh target; existing-data upgrades are unsupported" (`crates/control/lib/src/delivery/deployment.rs:474-516`). |
| Hand record | `docs/operations/gcp.md` section 7 holds one entry: `registry.capture_gap` and its grant, applied to `wamn_system` on 2026-09-29 as `wamn_system` (`wamn-59z6`). |
| Planned hand statements | `docs/plan/kind-to-type.md` sections 4.3.3 and 4.3.4 plan two scripts as the `postgres` superuser. The `wamn_system` script renames P1 to P7, replaces `identity.lock_password_principal`, and appends `record-history.sql`. The project script renames `kind` to `type` and two constraints on every `%_history` table, 15 tables in two databases, and appends `record-history.sql`. The `reconcile-run-plane` cutover of its section 4.3.2 carries P8 and P10 to P12. |

## 4. Design

### 4.1 The files and the records

System migrations live in `deploy/sql/migrations/system/`, and project migrations in `deploy/sql/migrations/project/`. Each file runs whole, with no `BEGIN` or `COMMIT` of its own.

`wamn_system` records its migrations in `registry.schema_migrations`: `ordinal`, `relative_path`, `sha256` and `applied_at`, with the checks of `catalog.package_migrations`. A project-environment database records them in `app_system.schema_migrations`, with the same columns. `registry.meta.schema_version` stays as it is.

In a fresh install, `provision-system` and `reconcile-run-plane` record every file as applied.

### 4.2 The verb

`wamn-ctl upgrade-schema`, in `crates/control/lib`, because the worker of `wamn-zua8` runs it and the production image builds `wamn-ctl` without features.

| Flag | Variable | Use |
| --- | --- | --- |
| `--system-database-url` | `WAMN_SYSTEM_ADMIN_URL` | Superuser URL of `wamn_system`. With it, the verb upgrades the system database. |
| `--admin-database-url` | `WAMN_PG_ADMIN_URL` | Superuser URL of one project-environment database. With it, the verb upgrades that database. |
| `--confirm` | | Without it, the verb prints the pending files and changes nothing. |

The verb takes one of the two URLs per run. It locks the record table, reads the recorded prefix, checks it against the files, and applies the pending suffix in order. System files run as `wamn_system`, as `provision-system` runs the full files. Project files run as the admin connection, as `reconcile-run-plane` runs `app-schema.sql`.

### 4.3 Refusals

- A recorded file that differs from its file on disk, by path or by sha256.
- A recorded ordinal with no file.
- No record table and no `--baseline`. The database was installed before the table existed (section 4.5).
- `--baseline` on a database whose record table has a row.
- A project URL whose database is not in `registry.project_envs`, or a system URL whose database is not `wamn_system`.

### 4.4 Ordering with releases

A migration changes the schema that the running binaries read. The order is:

1. If the change breaks a running binary, stop that binary first. A rename breaks every binary that names the old column. An added table breaks none.
2. Run `upgrade-schema` on `wamn_system`, then on each project-environment database.
3. If the release needs them, run `reconcile-run-plane` and `apply-package` of the new commit.
4. Start the new binaries, then publish and select the release as today.

The verb never runs a migration that `publish-release` or `deploy-release` needs, and neither verb runs `upgrade-schema`. `deploy-release` keeps its refusal for a schema-changing package, because package upgrades stay in `docs/plan/upgrades.md`.

### 4.5 The installed databases of wamn-dev

`wamn_system` and the two project databases have no record table. The first run on such a database takes `--baseline <ordinal>`, and only the first run. The operator states the last file that the database already holds. The verb creates the record table and records the files up to that ordinal without running them, then applies the rest.

### 4.6 The hand statements of section 7 under this verb

Each item says what the hand statement becomes under the verb.


- `registry.capture_gap` is `deploy/sql/migrations/system/0001_capture_gap.sql`: the `CREATE TABLE` block and the `GRANT SELECT` to `wamn_registry_reader`. `wamn-ctl upgrade-schema --system-database-url "$WAMN_SYSTEM_ADMIN_URL" --confirm` applies it. The `gcp.md` entry names the verb run and its time, not the statements.
- The `wamn_system` script of `kind-to-type.md` section 4.3.4 is `deploy/sql/migrations/system/0002_kind_to_type.sql`: the P1 to P7 renames, the new `identity.lock_password_principal`, and the record-history functions.
- The project script is `deploy/sql/migrations/project/0001_kind_to_type.sql`: the rename loop over every `%_history` table and the record-history functions. It runs once per project database.
- The P8 and P10 to P12 cutover stays in `reconcile-run-plane` for the cutover. The first project migration of `upgrade-schema` is the baseline after the cutover. The component key and the snapshot table of section 4.3.6 are migrations too.
- The order of `kind-to-type.md` section 4.7 stays. Steps 2 and 3 become two runs of `upgrade-schema` for each database.
- Rollback is a new migration with the names swapped. The verb never runs a file backward.

## 5. Issues

One branch. Each issue lands with its tests.

1. The record tables, the prefix check and the fresh-install record, in `provision-system` and `reconcile-run-plane`.
2. The verb, its refusals and `--baseline`. Live test on a disposable Postgres 18: a fresh install records every file. A new file applies once, and a second run changes nothing. An edited applied file refuses. A failing file leaves no change and no record.
3. `registry.capture_gap` as `0001_capture_gap.sql`. A run on wamn-dev with `--baseline 1` records it without running it. The `gcp.md` section 7 entry says so.
4. Closeout. The operations page gets the verb. `kind-to-type.md` sections 4.3.3, 4.3.4 and 4.8 name migrations in place of hand statements. Close `wamn-o8b9`.

## 6. Out of scope

- Package migrations of an application and `deploy-release` on an installed database (`docs/plan/upgrades.md`).
- A migration that runs backward.
- A change that needs a drain, a backfill in steps, or a second writer.
- The Kubernetes objects of a change, such as the PAT Secret annotations of `kind-to-type.md` section 4.3.5.

## 7. Owner rulings

The owner answered these on 2026-09-30 (recorded on `wamn-o8b9`).

1. A project schema change means the platform schemas of a project database. Package migrations stay in `docs/plan/upgrades.md`.
2. The names are accepted: `upgrade-schema`, `deploy/sql/migrations/system/`, `deploy/sql/migrations/project/`, `registry.schema_migrations` and `app_system.schema_migrations`.
3. `--baseline <ordinal>` is for the first run only. It is refused once the record table has a row.
4. The P8 and P10 to P12 renames stay in `reconcile-run-plane` for the cutover. The first project migration of `upgrade-schema` is the baseline after the cutover. A P3 finding (`wamn-qpgq`) says that the in-code schema changes of `reconcile-run-plane` move under `upgrade-schema` later.
