# Metadata Catalog Model (3.1) — canonical `0.1`

The canonical, versioned representation of a project's data model. **The catalog
is data, not DDL**: a set of entities, each with typed fields plus indexes and
constraints, wired by relations. It is the model the DDL compiler (3.2) turns
into migrations, the generated API (4.1) exposes as CRUD, the visual designer
(3.3) edits, and the RLS builder (3.5) attaches policies to.

**Neutral primitives only (D14).** The core catalog knows entities, fields,
types, relations, and constraints — not receipts, lots, or holds. Opinionated
domain models (a unified lot/serial treatment, an asset/historian model) are
optional modules layered on top; a client whose ontology disagrees swaps the
module, not the platform.

- **Issue:** wamn-it8 `[3.1]`; **Epic:** E3 Schema Designer.
- **Contract file:** [`catalog-model.schema.json`](catalog-model.schema.json) —
  the language-neutral JSON Schema, **generated** from the Rust types (single
  source of truth) and drift-guarded by a test.
- **Crate:** `crates/wamn-catalog` — types, import/export, validation, diff.
- **Storage:** [`deploy/sql/catalog-schema.sql`](../deploy/sql/catalog-schema.sql) — the
  tenant-scoped catalog tables that persist the model (a standalone artifact 3.2
  / POC-DM1 wire into a project DB; not part of the S2–S6 fixtures).
- **Consumers:** the DDL compiler (3.2), generated API (4.1), designer UI (3.3),
  RLS builder (3.5), schema-impact analysis (11.8).

## Model

A `Catalog` is **one version** of a project's model (the unit stored, versioned,
and promoted between environments, 3.4):

| Field | Type | Notes |
|---|---|---|
| `schema-version` | string | Catalog-model **format** version, e.g. `"0.1"`. Distinct from `version`. |
| `catalog-id` | string | Stable across every version (typically per project). |
| `version` | u32 | Monotonic version (≥ 1). |
| `name` | string? | Editor label. |
| `entities` | Entity[] | The tables of the model. |
| `relations` | Relation[] | Navigational relations between entities. |

Every entity is assumed to carry a **platform-managed surrogate primary key**
(an `id`, injected by the DDL compiler 3.2). References therefore target an
*entity*, not a named column, and natural keys are expressed as `unique`
constraints.

**Entity** — `{ id, name, is-system?, label?, description?, fields, indexes?,
constraints? }`. `id` is a stable logical slug; the DDL compiler maps `name` to
a physical table name.

**Field** — `{ id, name, type, nullable?, default?, sensitive?, is-system?,
label?, description? }`. `id` is stable across renames (so a rename is a *change*
in the diff, not a drop + add). `nullable` defaults to `false` (NOT NULL —
nullability is stated explicitly). `default` is an opaque JSON literal / SQL
expression interpreted by the DDL compiler. `sensitive` is a neutral flag the
field-level mask (4.3) keys on (e.g. supplier pricing hidden from inspectors);
this crate does not enforce masking.

### Field types

The field type system lives in 3.1 (the designer, 3.3, is the palette UI over
it). It is **industrial-friendly by construction**: timestamps carry a time
zone, quantities are exact decimals with an optional unit, and **there is no
float type** — floats are disallowed for material quantities and formulations.

| `kind` | Params | Notes |
|---|---|---|
| `text` | `max-len?` | Variable-length text, optionally length-capped. |
| `int` | — | 32-bit signed integer. |
| `big-int` | — | 64-bit signed integer. |
| `bool` | — | Boolean. |
| `uuid` | — | UUID. |
| `json` | — | Arbitrary JSON document (`jsonb`). |
| `date` | — | Calendar date (no time). |
| `timestamptz` | — | Instant with time zone — the only timestamp type. |
| `enum` | `variants` | Fixed set of string variants. |
| `numeric` | `precision`, `scale`, `unit?` | **Exact decimal**; optional unit (`kg`, `pct`, …). Floats are not representable. |
| `reference` | `entity` | Foreign key to another entity's managed primary key. |

### Relations

**Relation** — `{ id, name, cardinality, from, to, from-field?, through?,
description? }`. A relation is navigational metadata over the physical foreign
keys (a `reference` field *is* the FK column); the API generator (4.1) uses it
for nested expansion and the ERD (3.3) to draw edges.

- `cardinality` = `one-to-many` | `many-to-many` | `hierarchical`.
- `from` is the owning / child side (holds the FK for `one-to-many`); `to` is the
  referenced / parent side. `from-field` names the backing `reference` field.
- `many-to-many` names a join entity in `through`.
- `hierarchical` is a **self-referential tree** (`from == to`) — the
  closure / genealogy / asset-tree shape D14 requires industrial modules to be
  able to express.

### Indexes & constraints

**Index** — `{ name, fields, unique? }` over an entity's fields.

**Constraint** — a `kind`-tagged table-level constraint:
- `unique` `{ name, fields }` — composite (or single-column) uniqueness (the
  POC's `(receipt-no, supplier-id)`).
- `check` `{ name, expression }` — a boolean check, interpreted by the DDL
  compiler.

Opinionated domain constraints belong in modules (D14), not the core.

### System entities & extension

A **system entity** (`is-system`) is provided by the platform (e.g. `users`).
Its **system fields** (`is-system` on the field) are structure-locked — the
designer may not drop or retype them — but the entity stays **extensible**: a
project may add its own custom (non-system) fields. The POC's `users.cert_level`
is exactly this hard path. A system field on a non-system entity is
contradictory and rejected by validation.

### Example (canonical JSON)

```json
{
  "schema-version": "0.1",
  "catalog-id": "demo",
  "version": 1,
  "entities": [
    {
      "id": "users", "name": "users", "is-system": true,
      "fields": [
        { "id": "email", "name": "email", "type": { "kind": "text", "max-len": 320 }, "is-system": true },
        { "id": "cert_level", "name": "cert_level", "type": { "kind": "enum", "variants": ["L1", "L2"] }, "nullable": true }
      ]
    },
    {
      "id": "materials", "name": "materials",
      "fields": [
        { "id": "name", "name": "name", "type": { "kind": "text" } },
        { "id": "moisture_max_pct", "name": "moisture_max_pct", "type": { "kind": "numeric", "precision": 5, "scale": 2, "unit": "pct" } }
      ]
    }
  ]
}
```

The full POC data model (`users` extension, exact-decimal specs, the sensitive
pricing field, the `(receipt-no, supplier-id)` composite unique, and the
receipt→line→material→hold→disposition relations) is the fixture
`crates/wamn-catalog/tests/fixtures/poc-receiving.catalog.json`; a
hierarchical / closure model is `genealogy.catalog.json`. Each is round-tripped,
validated, and checked against the published schema in
`crates/wamn-catalog/tests/catalog.rs`.

## Import / export

`Catalog::from_json` / `Catalog::to_json` are the canonical import/export — the
JSON promotion format for moving a model between dev/prod projects (3.4). Export
is pretty-printed; default-valued fields (`nullable: false`, empty
`indexes`/`constraints`/`relations`, absent options) are omitted, so exported
catalogs are minimal and re-import to an identical value (round-trip).

## Validation

`Catalog::validate` checks **structure** and returns typed `Issue`s with a
stable machine `code` and a JSON path. Severity: only `error` makes a catalog
invalid; `warning` flags designer-fixable smells.

- **Errors:** unsupported `schema-version`, empty `catalog-id`, `version < 1`,
  empty/duplicate entity id or name, empty/duplicate field id or name,
  **system field on a non-system entity**, numeric `scale > precision` or
  `precision < 1`, empty / duplicate-variant enum, `text` `max-len` of 0,
  **reference to an unknown entity**, unknown / duplicate / empty index,
  unknown / duplicate / empty constraint, empty check expression, duplicate
  relation id, relation endpoint (or `through`, or `from-field`) that does not
  resolve, a `hierarchical` relation that is not self-referential, and **a name
  (entity, field, index, or constraint) beginning `wamn_`** (case-insensitive) —
  that prefix is reserved for platform-generated identifiers (migration asides,
  CDC entity-map artifacts, run-schema objects), so a designer name that collides is
  rejected up front rather than at migration-compile time.
- **Warnings:** entity with no fields; `many-to-many` relation with no join
  entity.

It deliberately does **not** emit DDL, plan migrations, or evaluate check
expressions — that is the DDL compiler's job (3.2).

## Diff

`diff(old, new)` produces a structured `CatalogDiff` — entities added / removed /
changed, and within a changed entity, fields added / removed / changed (which of
name / type / nullability / default / sensitivity changed), plus index and
constraint set changes, entity-attribute changes, and relation added / removed /
changed. Field identity is the stable `id`, so a **rename surfaces as a change**,
not a drop + add.

This is the input to the DDL compiler's migration planning (3.2 — added/removed/
retyped fields become `ALTER`s) and to schema-impact analysis (11.8 — a staged
`quality-holds.status` rename flags the flow suites and generated types that
depend on it *before* any DDL applies).

## Storage

`deploy/sql/catalog-schema.sql` persists the model in tenant-scoped catalog tables
(`catalog.catalogs` / `entities` / `fields` / `relations` / `indexes` /
`constraints`), same security shape as the rest of the platform (one `wamn_app`
role, FORCE RLS on the `app.tenant` claim). Field `type` is stored as the
`FieldType` JSON — the crate stays the single source of truth for type
semantics, and 3.2 interprets that `jsonb` via the `wamn-catalog` types rather
than the SQL schema enumerating every variant. These are the *definitions*; the
DDL compiler (3.2) reads them and emits the actual project tables. The file is a
standalone artifact (not included by `deploy/sql/postgres-init.sql`).

## Versioning & compatibility

Two independent version numbers: a catalog's own `version` (monotonic per
`catalog-id`), and the model **format** `schema-version` (this document: `0.1`).
Compatibility mirrors the WIT and flow-schema freezes — `0.1.x` is
additive/clarifying only; any breaking change waits for `0.2`. The validator
rejects a `schema-version` with a newer major or minor than it implements.

## Regenerating the contract

```sh
cargo run -p wamn-catalog --example print-schema > docs/catalog-model.schema.json
```

`catalog.rs::committed_schema_matches_types` fails if the committed file drifts
from the Rust types.

## References

- Plan: `docs/platform-plan.md` §Epic 3 (3.1–3.5), D14 (neutral core catalog).
- POC data model: `docs/poc-material-receiving.md` (Data model, traceability).
- Flow schema (the sibling contract crate): `docs/flow-schema.md`.
