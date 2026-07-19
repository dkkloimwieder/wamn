# RLS Policy Builder (3.5)

Turns per-entity access rules tied to roles — **row ownership**, **role command
gates**, and **custom per-role predicates** — into Postgres Row-Level Security
policies. It **composes** the two shipped Epic 3 crates: the rules resolve
against the catalog model (3.1) and compile to a `wamn-ddl` migration plan (3.2),
layering on top of that crate's **tenant floor**.

- **Issue:** wamn-idu `[3.5]`; **Epic:** E3 Schema Designer.
- **Crate:** `crates/wamn-rls` — consumes `wamn-catalog` + `wamn-ddl`.
- **Consumers:** the designer UI (3.3, authors the rules), the migration engine
  (2.5, applies them), 11.8 (impact-analyzes a rule change).

## Composition with the tenant floor

3.2 emits the tenant floor on every generated table: a **permissive**
`<t>_tenant` policy that isolates rows by `current_setting('app.tenant')`.
Postgres combines multiple **permissive** policies with `OR` and **restrictive**
policies with `AND`. So a second *permissive* policy would **widen** access
(a row visible if *either* matches) and quietly break tenant isolation — which is
why every policy this crate emits is `AS RESTRICTIVE`: it can only **narrow**
access *within* a tenant, while the floor keeps tenant isolation intact.

```sql
-- 3.2 floor (permissive): tenant isolation — unchanged
CREATE POLICY "dispositions_tenant" ON "dispositions"
    USING (tenant_id = current_setting('app.tenant', true));

-- 3.5 (restrictive): narrows WITHIN the tenant
CREATE POLICY "dispositions_owner_0" ON "dispositions" AS RESTRICTIVE
    FOR ALL
    USING (COALESCE(current_setting('app.role', true), '') IN ('supervisor', 'admin')
           OR "inspector_id" = NULLIF(current_setting('app.user_id', true), '')::uuid)
    WITH CHECK (…same…);
```

## Session claims

The rules key on two session claims, the per-user/role counterparts of the
floor's `app.tenant`, injected by the Postgres plugin alongside it (4.2):

- `app.role` — the caller's application role. Read as
  `COALESCE(current_setting('app.role', true), '')`, so an **absent** claim
  compares as a non-match (deny) instead of propagating NULL.
- `app.user_id` — the caller's user id. Cast as
  `NULLIF(current_setting('app.user_id', true), '')::uuid`, so an unset/empty
  claim becomes NULL (ownership → deny) and never raises an `''::uuid` error.

**Deploy ordering (important):** these policies are inert-but-safe until the
plugin injects the claims. Without `app.role`, a write gate **denies** the gated
command; without `app.user_id`, an ownership policy **denies all rows**. Deploy
3.5 policies together with the claim injection (4.2). Every emitted operation
carries a `note` saying so.

## Rule vocabulary (v1)

| Rule | Compiles to |
|---|---|
| **RowOwnership** { entity, owner-field, exempt-roles } | `AS RESTRICTIVE FOR ALL`: `owner = app.user_id::uuid`, OR'd with an exempt-role check. The owner field must be uuid / reference. |
| **RoleCommands** { entity, grants: [{command, roles}] } | one `AS RESTRICTIVE FOR <command>` per grant, gating on `app.role IN (roles)`. `USING` for SELECT/UPDATE/DELETE, `WITH CHECK` for INSERT/UPDATE. |
| **RolePredicate** { entity, role, command, expression } | `AS RESTRICTIVE FOR <command>`: `app.role <> '<role>' OR (<expression>)` — constrains only that role; the expression is emitted **verbatim** (the author owns its *logic*, but a statement-chaining fragment is rejected at validate time — see below). |

Reads (`SELECT`) stay open within the tenant floor unless a rule targets them, so
absent claims still allow tenant-scoped reads while denying gated writes.

## API

```rust
use wamn_rls::{AccessPolicy, compile, Confirmation};

let plan = compile(&policy, &catalog)?;   // -> a wamn-ddl MigrationPlan
plan.is_additive();                        // policy creation loses no data (always true in v1)
plan.report();                             // per-op review + the claim-dependency note
plan.sql(Confirmation::None)?;             // the CREATE POLICY script
```

`compile` runs the policy through validation first (entities resolve, ownership
fields are uuid-typed, roles/expressions non-empty, names unique), returning
`CompileError::InvalidPolicy` rather than emitting unsafe SQL. Output is a 3.2
`MigrationPlan`, so all creation is additive and needs no confirmation — the note
conveys that a new restriction can still deny access until claims flow.

**RolePredicate expression safety (cjv.5).** A `RolePredicate` expression is
spliced verbatim into `… OR (<expression>)` and applied through the simple
protocol, so a fragment like `true); DROP TABLE app_system.users; --` would chain
statements at migration-role privilege. Validation therefore rejects
(code `unsafe-expression`) any expression that carries a top-level `;`, unbalanced
parentheses, or a comment-open — the statement-chaining vectors — via the shared
`wamn_catalog::unsafe_expression_reason` scanner (literal-aware, so a `;` inside a
string stays legal). The author owns the predicate's *logic*, not the right to
append statements; the mirror guard on catalog `Check` expressions lives in 3.2
(`docs/ddl-compiler.md`).

## Storage

`deploy/sql/catalog-schema.sql` gains `catalog.rls_policies` (tenant-scoped, FORCE
RLS): one row per rule, the `rule` stored as jsonb (the crate is the source of
truth for its semantics). Policies attach to the live schema, so they are keyed
by `(tenant_id, catalog_id, policy_id)` — not to a specific catalog *version*.

## Scope

This crate **emits and classifies** RLS policies. It does not execute them (the
live apply is the migration engine 2.5 / hosting), inject the session claims (the
Postgres plugin 2.2 / 4.2), authenticate users (8.1), or model field-level
read/write masks (4.3). The tenant floor stays with 3.2 — 3.5 adds only the
per-role / ownership layer.

## Verification

```sh
cargo test -p wamn-rls
cargo clippy -p wamn-rls --all-targets && cargo fmt -p wamn-rls --check
```

Deterministic tests assert the emitted SQL shape (restrictive, per-command
clauses, safe claim coercions) and validation over the POC catalog. An optional
live-apply test applies the tenant floor + a compiled ownership policy to a
throwaway Postgres and asserts the policy actually filters rows (an owner sees
only their own rows, an exempt role sees all, an absent user claim denies all),
gated on `WAMN_RLS_PG_URL` (a superuser URL):

```sh
docker run -d --rm --name wamn-rls-pg -p 5453:5432 -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
WAMN_RLS_PG_URL=postgres://postgres:postgres@127.0.0.1:5453/wamn cargo test -p wamn-rls
docker stop wamn-rls-pg
```

## References

- Plan: `docs/platform-plan.md` §Epic 3 (3.5), §Epic 8 (8.2 tenant isolation).
- Catalog model (the input): `docs/catalog-model.md`, `crates/wamn-catalog`.
- Tenant floor (what these compose on): `docs/ddl-compiler.md`, `crates/wamn-ddl`.
- Claim injection: `docs/wamn-postgres.wit`, the wamn:postgres plugin (2.2 / 4.2).
