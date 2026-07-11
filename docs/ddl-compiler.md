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
`CompileError` rather than emitting unsafe DDL.

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

## Verification

`cargo test -p wamn-ddl` checks emitted SQL for the POC catalog (tenant floor,
composite unique, enum checks, unit comments, FKs) and the safety gate. An
optional live-apply test runs the emitted CREATE + an additive migration + a
confirmed destructive migration against a throwaway Postgres, gated on
`WAMN_DDL_PG_URL` (a superuser URL; the harness provisions the `wamn_app` role
and an ephemeral schema):

```sh
docker run -d --rm --name wamn-ddl-pg -p 5451:5432 -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
WAMN_DDL_PG_URL=postgres://postgres:postgres@127.0.0.1:5451/wamn cargo test -p wamn-ddl
docker stop wamn-ddl-pg
```

## References

- Plan: `docs/platform-plan.md` §Epic 3 (3.2), 2.5 (migration engine), D14.
- Catalog model (the input): `docs/catalog-model.md`, `crates/wamn-catalog`.
- Tenant shape: `deploy/postgres-init.sql`, `docs/security-db-path.md`.
