# Migration engine (2.5)

The **live executor** that applies a catalog to a project database:
versioned, forward-only, with a dry-run and a generated rollback plan. Shipped as
the pure crate `crates/wamn-migrate` (the engine) + the `wamn-ctl
migrate-catalog` subcommand (the effect shell), bead wamn-d8u,
`docs/platform-plan.md` §2.5.

## What it is — a thin executor over shipped machinery

2.5 does not re-derive migration logic. It **composes** the crates that already
emit and classify DDL and own the lifecycle:

- **3.2 `wamn-ddl`** — `Migration::create` / `migrate` compute the ordered,
  name-reuse-safe DDL plan, and the `Confirmation` gate refuses a destructive
  plan without a confirmed backup (prefixing a `-- BACKUP CHECKPOINT REQUIRED`
  marker). The engine reuses both **verbatim**.
- **3.4 `wamn-schema`** — the `draft → staged → applied → superseded` lifecycle
  with the *single-applied* and *stale-base* guards. The engine constructs an
  in-memory `Environment` mirroring the DB state and calls `apply()` as the
  **validation oracle**, so it can never diverge from the 3.4 semantics.
- **3.1 `wamn-catalog`** — the canonical model and its JSON, which the engine
  stores as the applied catalog **document** and diffs a target against.

Given the current applied catalog (read from the DB by the driver) and a target,
the engine produces:

- an **`ApplyPlan`** — the ordered `$n`-parameterized statements to run in **one
  transaction**;
- a **`MigrationReport`** — a dry run (no gate, no mutation) with the DDL report
  and the rollback plan;
- a **`RollbackPlan`** — a generated inverse forward-migration + a
  restore-to-last-dump pointer.

## The apply plan

For a migration from the current applied version *N* to a target *M* (`N = None`
for a first materialization), the plan runs inside one transaction:

1. **DDL** — `Migration::create(target)` (first materialization) or
   `migrate(current, target)` (a diff). A param-free multi-statement batch;
   omitted for a metadata-only version bump (empty diff).
2. **demote** — `UPDATE catalog.catalogs SET state='superseded'` for the current
   applied version (before the promote, so the single-applied partial-unique
   index is never transiently violated).
3. **promote** — upsert the target as `applied`, storing its catalog `document`
   (the diff source the next migration reads back).
4. **history** — append an immutable row to `catalog.schema_migrations`.

The engine emits `$n`-parameterized SQL (SR3); the driver holds the connection
and binds. Identifiers are pinned to the fixed `catalog` metadata schema; the
lifecycle-state literals come from `wamn_schema::State`, single-sourced with the
`catalog.catalogs` `CHECK`.

## Guards

- **forward-only** — `target.version` must be **newer** than the current applied
  version (`AlreadyApplied` when equal, `NotForward` when older). Versions are
  globally unique per catalog and apply only advances.
- **catalog-id** — the current and target must track the same catalog.
- **stale-base** — the target's `base` (the applied version it was branched from,
  `--base`, defaulting to the current applied) must equal the actual current
  applied version, else `StaleBase` — the 3.4 rebase guard, reused.
- **destructive** — a plan that drops/retypes is refused unless
  `--confirm-with-backup` (the 3.2 gate).

## Storage

Two additive changes to the standalone `deploy/sql/catalog-schema.sql` (not
`postgres-init.sql`):

- `catalog.catalogs` gains a nullable **`document jsonb`** column — the applied
  catalog JSON, the diff source. 2.5 is the **first live writer** of
  `catalog.catalogs` (publish-catalog writes a separate `wamn_catalog` snapshot;
  3.4 is pure in-memory).
- a new **`catalog.schema_migrations`** table — the immutable, forward-only apply
  journal: one row per applied migration `(from → to)`, its destructive flag,
  confirmation, operation count, and a checksum of the applied DDL. Under the 3.2
  tenant floor + a45 hardening; `wamn_app` is granted `SELECT, INSERT` only
  (append-only). The PK `(tenant, catalog, environment, to_version)` forbids
  recording a version twice.

## The one-transaction invariant (R9c)

The **whole** apply plan runs in one transaction. This is what makes the wamn-ddl
name-freeing preamble's **zero-residue** guarantee hold: a mid-plan failure rolls
the aside-renames (`wamn_mig_drop_*`) back, so nothing survives — no compensation
path is needed.

This holds **while the compiler emits no non-transactional step**. The known
breaker is `CREATE INDEX CONCURRENTLY` (it cannot run inside a transaction
block). The current emitter emits plain `CREATE INDEX` inside the plan, so v1 is
safe. When a non-transactional step is introduced, 2.5 must grow (a) a **residue
janitor** — sweep `wamn_mig_drop_*` older than a grace with no in-flight
migration — and (b) an **apply journal** to resume/repair a partially-applied
plan. Deferred and filed as a follow-up bead; v1 adds no such step.

## Rollback

Forward-only means there are no down-scripts. The generated rollback is an
**inverse forward-migration** — `migrate(target, current)`, a new forward plan
back to the prior version. It is itself destructive (it drops the migration's
additions), so it carries the 3.2 gate. For data a forward rollback cannot
recover (rows written under a dropped column/table), the plan points at
**restore-to-last-dump** (wamn-q3n.11). A first materialization has no prior
version: the rollback is a drop / restore, not a generated inverse.

## The subcommand

```
wamn-ctl migrate-catalog \
  --admin-database-url <superuser URL to the project DB> \
  --tenant <tenant> --environment dev|canary|prod --schema <data schema> \
  --target <catalog.json> [--base <n>] [--dry-run] [--confirm-with-backup]
```

Connects as a superuser (the DDL creates tables/policies/grants, like
`publish-catalog --provision`); ensures the data schema exists; reads the current
applied version (locked `FOR UPDATE`); then either prints the dry-run report or
executes the plan in one transaction. `--dry-run` touches nothing.

## Scope (v1)

The **tenant catalog** migration engine — what unblocks POC-DM1 (define a catalog
→ migrate live → get data tables). The "system-schema migrations shipped with
platform releases" flavor (hand-written SQL evolving `app_system` / `catalog`
across every project DB on upgrade — different inputs, different trigger) is a
separate follow-up. Shredding the applied catalog into the 3.1
`catalog.entities/fields/…` content rows (for the 3.3 designer / 11.8 impact
analysis) is a follow-up too; v1 stores the applied catalog as the `document`.

## Verification

- **Unit** (`cargo test -p wamn-migrate`): the guards, the 3.2 destructive gate,
  dry-run vs apply, the generated rollback, and a metadata-only version bump.
- **Drift guard**: `deploy/sql/catalog-schema.sql` must mirror the engine — the
  `document` column, the `schema_migrations` table + columns, and the
  confirmation / environment / lifecycle-state literals the SQL builders use.
- **Live-apply gate** (`WAMN_MIGRATE_PG_URL`, a superuser URL; skipped when
  unset): a first materialization, a forward migration (document round-trip,
  single-applied advance, history), and a gated destructive migration, over a
  real Postgres.
- **Mutation** (`scratchpad/mutate_d8u.py`): five mutants (forward-only guard,
  stale-base validation, the backup gate, the demote state literal, the history
  statement) each fail a named test.

Nothing in-cluster — an engine + schema is proven by a throwaway Postgres (the
`catalog-schema.sql` / wamn-ddl / wamn-schema precedent); applying it in-cluster
would mutate a shared DB (the shared-cluster guardrail).
