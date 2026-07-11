# Seed-Data & Fixtures Tooling (3.6)

Reference and fixture data for a catalog (3.1) as a typed **dataset** —
rows grouped by entity, each identified by a **symbolic key** and referencing
other rows by key — compiled to tenant-scoped, **idempotent** `INSERT`s against
the tables the DDL compiler (3.2) generates.

- **Issue:** wamn-x71 `[3.6]`; **Epic:** E3 Schema Designer.
- **Crate:** `crates/wamn-seed` — consumes `wamn-catalog` + `wamn-ddl`.
- **Consumers:** POC-DM1 (load the reference data), the test host (11.1, which
  clones a schema "with system schema + seed data"), preview environments (11.9,
  masked seed data), the control plane.

## Model

```jsonc
{
  "schema-version": "0.1",
  "catalog-id": "poc-material-receiving",
  "entities": [
    { "entity": "suppliers",
      "rows": [ { "key": "acme", "values": { "name": "Acme", "standard-cost": "12.50" } } ] },
    { "entity": "receipts",
      "rows": [ { "key": "r1", "values": { "receipt_no": "R-001", "supplier_id": "acme" } } ] }
  ]
}
```

A row's `key` is unique within its entity. A `reference` field's value is the
**key** of the target row (`"supplier_id": "acme"`), not a uuid — the compiler
resolves it. The managed `id` / `tenant_id` columns are never set in `values`.

## Deterministic ids

Each row's managed `id` is `uuidv5(namespace, "tenant:entity:key")`. This makes:

- **references resolve at compile time** — a reference to `acme` becomes the
  exact uuid the `suppliers:acme` row is inserted with;
- **re-seeding stable** — the same dataset compiled for the same tenant yields
  byte-identical SQL, so `ON CONFLICT (id) DO NOTHING` makes a repeated load
  (a test host re-cloning a schema, a re-seed) a no-op;
- **tenants distinct** — the tenant is part of the derivation, so two tenants
  seeding the same keys get different ids (the `id` primary key is global).

## Compilation

```rust
use wamn_seed::{Dataset, compile, Confirmation};

let plan = compile(&dataset, &catalog, "tenant-a")?; // -> a wamn-ddl MigrationPlan
let sql  = plan.sql(Confirmation::None)?;            // a seed load is all-additive
```

`compile` validates first (types, references, required fields, uniqueness),
returning `CompileError::InvalidDataset` rather than emitting broken SQL. It then
emits one `INSERT … ON CONFLICT (id) DO NOTHING` per row, entities in
**foreign-key-safe order** (a referencing entity after the entities it points
at; a reference *cycle* falls back to author order). Values render per field
type; a `numeric` is emitted as an **exact-decimal literal** (never a float —
the 3.1 rule holds end to end).

## Validation

`validate(&dataset, &catalog)` (reusing the catalog's `Issue` / `Severity`)
checks: schema-version compatibility, catalog match, entities/fields resolve,
no reserved (`id` / `tenant_id`) columns, values match field types (enums are
variants, uuids parse, numerics are exact decimals that fit `numeric(p,s)`),
references point at seeded keys, required (non-nullable, un-defaulted) fields are
present, and per-entity keys plus composite-unique tuples are distinct.

## Storage

`deploy/catalog-schema.sql` gains `catalog.seed_datasets` (tenant-scoped, FORCE
RLS): one row per dataset, the `dataset` document stored as jsonb (the crate is
the source of truth for its semantics; the compiler emits the INSERTs from it).

## Scope

This crate **emits and classifies** seed SQL. It does not apply it (the live
load is the migration engine 2.5 / hosting / the test host 11.1), pin
record-and-replay run fixtures (11.3), or mask sensitive seed data for preview
environments (11.9) — though it carries the catalog's `sensitive` flag so 11.9
can. The generated tables are 3.2's; this crate only populates them.

## Verification

```sh
cargo test -p wamn-seed
cargo clippy -p wamn-seed --all-targets && cargo fmt -p wamn-seed --check
```

Deterministic tests assert the emitted SQL (FK-safe order, deterministic ids,
reference resolution, exact-decimal literals, idempotent conflict clause) and
validation over the POC catalog. An optional live-apply test loads a compiled
seed into a throwaway Postgres, re-applies it, and asserts the foreign key
resolves and the second load is a no-op — gated on `WAMN_SEED_PG_URL`:

```sh
docker run -d --rm --name wamn-seed-pg -p 5454:5432 -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
WAMN_SEED_PG_URL=postgres://postgres:postgres@127.0.0.1:5454/wamn cargo test -p wamn-seed
docker stop wamn-seed-pg
```

## References

- Plan: `docs/platform-plan.md` §Epic 3 (3.6), §Epic 11 (11.1 test host, 11.9 previews).
- Catalog model (the input): `docs/catalog-model.md`, `crates/wamn-catalog`.
- Tenant floor (the target tables): `docs/ddl-compiler.md`, `crates/wamn-ddl`.
