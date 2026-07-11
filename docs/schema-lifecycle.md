# Schema Versioning & Environments (3.4)

A catalog does not go straight from edited to live. Each version moves through a
**lifecycle** — `draft → staged → applied` (with `superseded` for prior applied
versions) — and is **promoted** between **environments** (`dev`, `prod`). This
crate owns that lifecycle and promotion policy. It **composes** the two shipped
Epic 3 crates rather than duplicating them:

- [`wamn-catalog`](catalog-model.md) (3.1) — the canonical model, its version
  `diff`, and the JSON import/export that *is* the promotion format;
- [`wamn-ddl`](ddl-compiler.md) (3.2) — the DDL compiler and its
  additive/destructive confirmation gate, reused verbatim to compile a
  promotion's migration.

- **Issue:** wamn-d6d `[3.4]`; **Epic:** E3 Schema Designer.
- **Crate:** `crates/wamn-schema` — depends on `wamn-catalog` + `wamn-ddl`.
- **Consumers:** the designer UI (3.3, drives the lifecycle), the migration
  engine (2.5, applies a promotion's plan), 11.8 (impact-analyzes a staged
  version before apply), the control plane (records versions per environment).

## Scope

This crate is the **lifecycle + promotion model**. It does **not** execute DDL,
keep a versioned migration history, or roll back — that is the migration engine
(2.5), which wraps a promotion's `MigrationPlan`. The real backup / PITR
mechanism is hosting (2.3 / 10.3); the draft-editing designer UI and the staging
screen are 3.3; per-role RLS is 3.5. Version *storage* lives in
[`deploy/catalog-schema.sql`](../deploy/catalog-schema.sql); this crate is the
in-memory model that storage persists.

## Lifecycle

```text
  Draft ──stage──▶ Staged ──apply──▶ Applied ──(superseded on next apply)──▶ Superseded
    ▲                 │
    └─────unstage─────┘
  Draft / Staged ──discard──▶ (removed)
```

| State | Meaning |
|---|---|
| **Draft** | Editable; the only state whose catalog content 3.3 may mutate. |
| **Staged** | A frozen candidate, awaiting apply (impact-analyzed by 11.8). |
| **Applied** | The live schema. Exactly one per (environment, catalog). |
| **Superseded** | A previously-applied version, kept as history. |

`transition(from, action)` is the **pure** legal-transition table (no
cross-version context). Two invariants that *do* need cross-version context are
enforced by `Environment`:

- **single-applied** — at most one `Applied` version per environment; applying a
  Staged version demotes the previous Applied to `Superseded`.
- **stale-base (rebase) guard** — a Staged version records the applied `base` it
  was branched from, and may be applied only while that base is *still* the
  current Applied. If someone applied a newer version in the meantime, the stale
  candidate is refused (`StaleBase`) until it is rebased — concurrent-change
  safety.

## Environments

An **environment** is a deployment target; in the per-project-database model
(2.2 / 2.3) it is a project's database. Version numbers are **globally unique per
catalog** (promotion mints a fresh version in the target environment), so
`environment` is an *attribute* of each version rather than part of its identity.

```rust
use wamn_schema::{Environment, promote, Confirmation};

let mut dev = Environment::new("dev", &catalog.catalog_id);
dev.add_draft(catalog, None)?;   // first version (no base)
dev.stage(1)?;
dev.apply(1)?;                   // now live in dev

let prod = Environment::new("prod", dev.catalog_id());
let plan = promote(&dev, &prod)?;         // prod empty -> a fresh CREATE
let sql = plan.sql(Confirmation::None)?;  // additive: no confirmation needed
```

## Promotion

`promote(source_env, target_env)` diffs the target environment's current applied
catalog against the source's applied catalog and compiles the migration, reusing
the 3.2 DDL compiler and its safety gate:

- target empty → `Migration::create` (a fresh, all-additive `CREATE`);
- target has an applied version → `Migration::migrate(target, source)` (a diff,
  which may be destructive).

The lower-level `promote_catalog(source, target_applied)` takes catalogs directly
(the same call, environment-independent). Both return a `PromotionPlan`:

```rust
plan.is_additive();                            // no destructive ops?
plan.requires_confirmation();                  // any destructive op?
plan.report();                                 // warnings + per-op additive/DESTRUCTIVE review
plan.sql(Confirmation::None)?;                 // Err(RequiresConfirmation) if destructive
plan.sql(Confirmation::ConfirmedWithBackup)?;  // prefixes a backup-checkpoint marker
```

`PromotionPlan` carries non-fatal `warnings` (a catalog-model version skew
between environments, or a source version that is not newer than the target's
applied version). Applying the plan and recording the new version in the target
environment is the caller's step — this crate stays pure and emits no DDL of its
own beyond what `wamn-ddl` produces.

## Storage

`deploy/catalog-schema.sql` persists the lifecycle on `catalog.catalogs`:

- `state text` — the lifecycle state (`draft` / `staged` / `applied` /
  `superseded`), generalizing the earlier `active` boolean. Its values are
  exactly `wamn_schema::State::as_sql`, tied to the crate by a test.
- `environment text` — the deployment target (first-class).
- `base_version int` — the applied version a draft/staged one was branched from
  (backs the stale-base guard).
- a partial unique index enforcing single-applied:
  `(tenant_id, catalog_id, environment) WHERE state = 'applied'`.

The rest of the tenant-scoped RLS shape is unchanged from 3.1.

## Verification

```sh
cargo test -p wamn-schema
cargo clippy -p wamn-schema --all-targets && cargo fmt -p wamn-schema --check
```

Tests cover the transition table, the single-applied and stale-base guards,
promotion (first `CREATE`, additive, gated destructive, environment-aware), and
a drift guard tying `State` to the storage `CHECK` in `catalog-schema.sql`. The
storage additions re-apply cleanly on a throwaway Postgres 18 (as with 3.1 / 3.2).

## References

- Plan: `docs/platform-plan.md` §Epic 3 (3.4).
- Catalog model (the promotion format): `docs/catalog-model.md`, `crates/wamn-catalog`.
- DDL compiler (reused for the migration): `docs/ddl-compiler.md`, `crates/wamn-ddl`.
- Storage: `deploy/catalog-schema.sql`.
