# DDL Compiler (3.2)

Turns the canonical catalog model (3.1, [`catalog-model.md`](catalog-model.md))
into Postgres DDL: a whole catalog into `CREATE` statements, or a catalog *diff*
into an ordered **migration plan** of `ALTER`s. Every operation is classified
**additive** or **destructive**; the plan applies additive changes freely but
**refuses destructive ones unless the caller confirms them and asserts a backup
checkpoint** — the "additive by default; destructive requires explicit
confirmation + backup checkpoint" rule (3.2).

- **Issue:** wamn-vbd `[3.2]`; **Epic:** E3 Schema Designer.
- **Crate:** `crates/wamn-ddl` — consumes `wamn-catalog::{Catalog, diff}`.
- **Consumers:** POC-DM1 (materialize the model), 3.4 (draft→staged→applied
  lifecycle), 11.8 (schema-impact analysis reads the plan's per-op entity/field
  attribution), the migration engine (2.5) wraps it for live apply.

## Scope

This crate **emits and classifies** DDL. It does **not** execute it. The live
transactional apply, versioned migration history, and rollback are the migration
engine (2.5); the real backup / PITR mechanism is hosting (2.3 / 10.3); the
draft→staged→applied lifecycle is 3.4; per-role RLS rules are 3.5. It **does**
emit the platform multi-tenancy floor so generated tables are tenant-safe from
the first `CREATE`.

## API

```rust
use wamn_ddl::{Migration, Confirmation};

let plan = Migration::create(&catalog)?;          // whole catalog -> CREATE (all additive)
let plan = Migration::migrate(&old, &new)?;       // diff -> ALTERs (may be destructive)

plan.is_additive();            // no destructive ops?
plan.requires_confirmation();  // any destructive op?
plan.report();                 // human review: each op tagged additive / DESTRUCTIVE + caveats
plan.preview_sql();            // full script, ungated (for review / impact analysis)
plan.sql(Confirmation::None)?; // Err(RequiresConfirmation) if destructive & unconfirmed
plan.sql(Confirmation::ConfirmedWithBackup)?; // prefixes a backup-checkpoint marker
```

`Migration::create` / `migrate` first run the catalog through 3.1 validation and
reject reserved managed-column collisions (`id` / `tenant_id`), returning
`CompileError` rather than emitting unsafe DDL. `migrate` additionally rejects a
table- or column-rename cycle (a swap) and an aside-name collision — see
*Migration ordering & name reuse* below.

Two further entry points emit the **outbox row-event triggers** (see the
dedicated section below):

```rust
use wamn_ddl::OutboxOptions;

let plan = Migration::outbox_triggers(&catalog, &OutboxOptions::default())?; // all additive
let plan = Migration::drop_outbox_triggers(&catalog)?;                       // destructive (gated)
```

## Generated table shape (the tenant floor)

Every entity becomes a table with a managed surrogate key and the S2 / 2.2
tenant-isolation shape:

```sql
CREATE TABLE "receipts" (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id text NOT NULL,
    "receipt_no" varchar(64) NOT NULL,
    "supplier_id" uuid NOT NULL,
    "received_at" timestamptz NOT NULL
);
ALTER TABLE "receipts" ENABLE ROW LEVEL SECURITY;
ALTER TABLE "receipts" FORCE ROW LEVEL SECURITY;
CREATE POLICY "receipts_tenant" ON "receipts"
    USING (tenant_id = current_setting('app.tenant', true))
    WITH CHECK (tenant_id = current_setting('app.tenant', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON "receipts" TO wamn_app;
```

All tables are created first, then foreign keys, constraints, indexes, and unit
comments are attached — so a foreign key never precedes its target table.
Uniqueness and indexes are **tenant-scoped** (`tenant_id` is prepended), e.g.
`ADD CONSTRAINT "receipts_no_supplier_uniq" UNIQUE (tenant_id, "receipt_no", "supplier_id")`.

## Type mapping

| catalog `type` | Postgres |
|---|---|
| `text` (`max-len` n) | `text` / `varchar(n)` |
| `int` / `big-int` | `integer` / `bigint` |
| `bool` | `boolean` |
| `uuid` | `uuid` |
| `json` | `jsonb` |
| `date` / `timestamptz` | `date` / `timestamptz` |
| `enum` | `text` + `CHECK (col IN ('a','b',…))` |
| `numeric` (`precision`,`scale`,`unit?`) | `numeric(p,s)` + `COMMENT … IS 'unit: …'` |
| `reference` | `uuid` + `FOREIGN KEY … REFERENCES <target> (id)` |

Enums compile to a text column with a `CHECK` (migration-friendlier than a
Postgres `enum` type). Units survive to the database as a column comment.
Expression defaults are a 0.2 item — a `default` is emitted as a SQL literal.

## Safety classification

| Change | Classification |
|---|---|
| create table, add column, add index, add constraint, add FK, drop non-unique index, drop NOT NULL, set/drop default, comment | **additive** |
| drop table, drop column, retype column, rename column, rename table, set NOT NULL, drop constraint, drop unique index | **destructive** |

Additive operations that can still fail on existing data (an `ADD COLUMN NOT NULL`
with no default, an `ADD CONSTRAINT` against violating rows, a `SET NOT NULL`
over NULLs) carry a `note` surfaced in `report()`. `plan.sql(Confirmation::None)`
returns `RequiresConfirmation` (listing the destructive summaries) for any
destructive plan; `ConfirmedWithBackup` prefixes the script with a
`-- BACKUP CHECKPOINT REQUIRED` marker the executor (2.5) must honor.

Relations are navigational metadata; only a `reference` **field** produces a
foreign key. A relation-only change emits no DDL.

## Migration ordering & name reuse

`Migration::migrate` orders operations additive-first / destructive-last —
**except a name-freeing preamble**. Postgres tables, indexes, and
unique-constraint backing indexes share one relation namespace (and column /
constraint names are table-scoped namespaces of their own), and a single
version bump may both *free* a name (rename / drop) and *reclaim* it (create /
add); the freeing side must execute first or the add fails (42P07 / 42710 /
42701) and the whole transactional apply (2.5) rolls back. Plan order:

1. **Dropped tables whose name is reclaimed are renamed aside**
   (`wamn_mig_drop_<name>`), together with their indexes — index names
   (including the implicit `<table>_pkey`) do **not** follow a table rename,
   so the recreated table's canonical index names would otherwise collide or
   silently drift. A doomed table whose *index* name (only) is reclaimed moves
   just those indexes aside. The actual `DROP TABLE` stays **last** (targeting
   the aside name), so the destructive tail's FK unwind order is untouched —
   foreign keys follow a rename. Every synthesized aside name is checked
   against the full relation namespace of both catalog versions
   (`CompileError::TempNameCollision`), conservatively: a collision rejects
   even when the holder is itself renamed away in the same bump.
2. **Hoisted constraint and index drops**: drops that free a reclaimed name —
   plus *all* constraint/index drops of an entity whose column drop is hoisted
   in step 4 (the `DROP COLUMN` implicitly drops objects involving the column,
   and a tail drop would then fail on the missing object). A same-name
   redefinition (same name, changed definition) diffs as drop + add and lands
   here. These run **before** the renames — a rename's own target may be a
   name freed only by such a drop — so a hoisted constraint drop references
   its table by the *pre-rename* name (`DROP INDEX` references no table name).
3. **All table renames**, dependency-ordered: a rename claiming a name freed
   by another rename runs after it (`a -> b` waits for `b -> c`). A cycle — a
   swap, `a -> b` **and** `b -> a` in one bump — has no rename-only order and
   is rejected (`CompileError::TableRenameCycle`); split it into two version
   bumps. Each rename takes its implicit pkey along
   (`ALTER INDEX IF EXISTS "<old>_pkey" RENAME TO "<new>_pkey"`): left stale,
   the recreated same-named table's pkey silently drifts to a suffixed name
   (Postgres auto-avoids implicit-name collisions rather than erroring) and a
   *later* migration's aside-rename of `<table>_pkey` would grab a live
   table's index. Hoisting *every* rename (not only name-freeing ones) also
   keeps the plan's later `ALTER TABLE <new name>` operations on the same
   entity valid.
4. **Per-entity column-namespace freeing** — the same rule one level down:
   a dropped column whose name is re-added (or claimed by a column rename)
   drops now, and all column renames run here, dependency-ordered within
   their table; a column-rename swap is rejected
   (`CompileError::ColumnRenameCycle`). These reference the table post-rename,
   hence after step 3.
5. The additive-first / destructive-last steps as before: creates + RLS,
   attachments, additive adds, column alters (retype / nullability /
   default), then the destructive tail. Within that tail the **removed tables
   drop dependents-first**: the diff yields `entities_removed` in entity-id
   lexical order, which is FK-blind, so they are topologically ordered on
   their inbound `Reference` edges *within the removed set* — a child holding
   an FK to a removed parent drops before it — mirroring the step-3 rename
   ordering. Otherwise `DROP TABLE parent` before its still-present child
   fails 2BP01 (`constraint <child>_<fk>_fkey depends on table <parent>`) and
   the one-txn apply rolls back. Only edges among removed tables matter: a
   reference to a *retained* table keeps its FK on the retained side, and the
   new catalog cannot retain a reference to a removed table (validation
   rejects the dangling target). A mutual-FK cycle among dropped tables has no
   drop-only order and is rejected (`CompileError::DropCycle`), as the rename
   cycles are; break it by dropping one side's reference field in an earlier
   bump.

Plans that free no reused name and rename nothing are unchanged by the
preamble, and hoisted operations keep their destructive classification — the
confirmation gate is order-independent. Known limits: index names that drifted
outside the catalog before this ordering existed (an index created under an
earlier table name keeps that name) are moved aside with `ALTER INDEX IF
EXISTS`, so a drifted source skips the move and a colliding claim fails loudly
at apply rather than silently — the pkey-follows-rename rule keeps names
canonical so drift no longer accumulates.

## Outbox row-event triggers (5.14 / D4 producers)

`Migration::outbox_triggers(&catalog, &OutboxOptions { schema })` emits the
production **row-event producers**: one shared plpgsql function plus one
`AFTER INSERT OR UPDATE OR DELETE ... FOR EACH ROW` trigger per entity table,
inserting one row into `<schema>.outbox` (default `wamn_run`,
[`deploy/run-queue.sql`](../deploy/run-queue.sql)) **inside the user's
transaction** — D4's "outbox insert and enqueue can share a transaction with
user writes": the event is durable iff the write it announces is. The trigger
dispatcher (5.14, `docs/run-queue.md`) polls these rows, matches
`(table_name, event)` against active `row-event` flows, and splices
`payload::text` into the run input **verbatim**.

Shape and invariants:

- **Event vocabulary** is `lower(TG_OP COLLATE "C")` → `insert|update|delete`,
  exactly the outbox `event` CHECK and the wamn-flow `row-event` strings;
  `TG_TABLE_NAME` is the physical table name row-event flows declare. The `"C"`
  collation pin matters: under a Turkish/Azeri database default collation,
  `lower('INSERT')` is `ınsert` (dotless ı), which would fail the CHECK and —
  the trigger sharing the user's transaction — abort the user write itself.
- **Tenant from the row, not the claim**: `NEW.tenant_id` / `OLD.tenant_id`
  (the tenant-floor column) — correct under superuser seeds, which carry no
  `app.tenant`. For a `wamn_app` write the entity floor's `WITH CHECK` already
  pinned the row's tenant to the claim, so the outbox policy passes by
  construction.
- **Payload**: `to_jsonb(NEW)` for insert/update, `to_jsonb(OLD)` for delete.
  Postgres jsonb numerics are exact, so an exact-decimal column (`12.50`)
  survives into the payload and from there verbatim into the run input — the
  no-float rule holds structurally end to end. An `ON CONFLICT DO NOTHING`
  no-op (a 3.6 re-seed) inserts no row and fires nothing; a *first* seed fires.
  Caveat: Postgres special values serialize as JSON *strings* (`'NaN'::numeric`
  → `"NaN"`, `'infinity'::timestamptz` → `"infinity"`); excluding them from
  entity columns is tracked follow-up validation work (wamn-oj7).
- **Runtime precondition**: the plan applies cleanly even where the outbox does
  not exist (plpgsql bodies are not plan-checked at `CREATE FUNCTION`) and
  fails only on the first subsequent row write — so the function operation's
  summary names the target (`… events -> "wamn_run"."outbox"`) and its note
  states the precondition, keeping a mis-targeted or schema-drifted apply
  visible on the plan review surface.
- **Opt-in and uniform**: a separate plan covering ALL entity tables — the
  dispatcher acks rows no flow is registered on cheaply. It is deliberately
  not folded into `create`/`migrate`: their consumers' schemas (3.4/3.5/3.6
  gates, the 4.1 gateway fixtures) have no outbox, and every row write would
  fail once a trigger references it. Provisioning composes both plans for
  project databases that carry the run schema.
- **Idempotent + rename-safe**: `CREATE OR REPLACE` on the function and a
  CONSTANT trigger name (`wamn_outbox_event`; trigger names are per-table)
  make the plan safe to re-apply on every catalog version — added entities
  gain their trigger, a renamed table keeps exactly one (the trigger follows
  the rename and re-apply replaces it instead of stacking a second), and
  `DROP TABLE` takes its trigger with it.
- **Classification**: all additive. The opt-out plan
  (`Migration::drop_outbox_triggers`) is destructive — no data is lost, but
  row-event flows on these tables silently stop firing — so it is gated
  behind `Confirmation::ConfirmedWithBackup`. Its final `DROP FUNCTION` is
  deliberately RESTRICT (no CASCADE): if a table *outside* the passed catalog
  still carries the trigger (version drift), it fails loudly rather than
  silently killing that table's events; re-run with the catalog version whose
  triggers were actually applied. The shared-function operations are
  catalog-scoped and carry an empty `entity` attribution.
- The `OutboxOptions::schema` must be a bare identifier
  (`[A-Za-z_][A-Za-z0-9_]*`) — it is embedded inside the function body's
  dollar-quoted block, where quoting cannot protect against a value containing
  the dollar tag — else `CompileError::InvalidOutboxSchema`.

`cargo run -p wamn-ddl --example emit-outbox -- <catalog.json> [schema]
[--create]` prints the plan (with `--create`, a complete provisioning script)
for demos and manual project setup.

## Verification

`cargo test -p wamn-ddl` checks emitted SQL for the POC catalog (tenant floor,
composite unique, enum checks, unit comments, FKs), the safety gate, the
migration ordering (name reuse via rename and via drop-and-re-add — reclaimed
by a create *and* by a rename — three-hop rename chains, swap and
aside-name-collision rejection incl. index aside-targets, same-named
constraint/index redefinition on kept and renamed tables, a rename into a
constraint-freed name, cross-table unique-name moves, pkey-follows-rename,
column-name reuse / column-rename chains and swap rejection, the
implicit-drop force-hoist, and collision-free plans staying preamble-free —
each ordering rule is mutation-tested), and the outbox-trigger plans
(coverage/shape, schema-option validation, the gated drop,
and a drift guard pinning the emitted column set + event vocabulary against
`deploy/run-queue.sql`). Three optional live-apply tests run against a throwaway
Postgres, gated on `WAMN_DDL_PG_URL` (a superuser URL; the harness provisions
the `wamn_app` role and ephemeral schemas): the CREATE/migrate script; the
name-reuse migrations applied for real (a rename into a reused name with other
changes on the same entity, a remove-and-re-add under the same table/index
names over a live inbound FK with canonical pkey names asserted on both sides,
in-place constraint/index redefinitions on kept and renamed tables, and a
same-named column redefinition — each failed 42P07 / 42P01 / 42710 / 42701
before the preamble); and the outbox triggers
behaviorally — a `wamn_app` write emits exactly one event row
in the same transaction with the exact-decimal payload preserved, a superuser
seed fires with the row's tenant, outbox RLS isolates tenants, a conflict no-op
emits nothing, a re-applied plan stacks no duplicate trigger, and the confirmed
drop plan silences emission:

```sh
docker run -d --rm --name wamn-ddl-pg -p 5451:5432 -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
WAMN_DDL_PG_URL=postgres://postgres:postgres@127.0.0.1:5451/wamn cargo test -p wamn-ddl
docker stop wamn-ddl-pg
```

## References

- Plan: `docs/platform-plan.md` §Epic 3 (3.2), 2.5 (migration engine), D14.
- Catalog model (the input): `docs/catalog-model.md`, `crates/wamn-catalog`.
- Tenant shape: `deploy/postgres-init.sql`, `docs/security-db-path.md`.
