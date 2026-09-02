# Data access — sqlx integration (rev 3, aligned to the base-application POC)

Status: DRAFT rev 3 · 2026-08-27 · aligned to
`wamn_base_application_poc_revised.md` + receiving scenario · supersedes
rev 2's generic-entity default · evidence: wamn-0h0g.22.13 (receipt
5c464ae). Scope: post-merge product work (Phase B item-4 successor);
nothing here rides the scope-reduction RC.

## Schema authority (one way, no DSL)

```text
migration SQL (per package, ordered stream)
  → PostgreSQL (the authority)
  → pg_catalog introspection (configured schemas, closed object set)
  → normalized WAMN catalog IR (derived artifact — never edited)
  → generated artifacts
```

The effective schema is exact platform migrations + exact client-overlay
migrations. The IR is a release artifact, not a schema-editing surface.

## The three layers (rev 2's table, corrected)

| Layer | Checking | Notes |
|---|---|---|
| **Application SQL corpus** — generated CRUD/lock/mutation SQL **plus authored named query/projection SQL** (`query/*.sql`) | **Compile-time, two-sibling**: the native verifier runs `query_file!`/`query_file_as!` against `sqlx::Postgres` on the exact effective schema and `cargo sqlx prepare --check` in CI; the wasm artifact sends a statement digest and binds, and the host resolves the pinned exact bytes through its existing claim-aware `WamnPostgres` runner (`wamn-10yt.9.2`) | Corpus identity welded into the package record |
| Tenant/developer + workflow-editor arbitrary SQL | **Runtime-checked** over `WamnPostgres`; gate + bounded execution | `.22.13` unchanged |
| External consumers | Generated routes → registered operations | CallerIdentity → permission check; host-selected DB identity → privileges/RLS |

**Structural invariant (unchanged):** verified SQL == executed SQL, by
one generated/authored file consumed by both targets. `.sqlx/` is build
evidence, never contract. **Type-parity refusal (unchanged):** generation
refuses PostgreSQL types outside the `wamn:postgres` value contract.

## Generated access — supersedes the generic entity

No universal runtime controller with `call(operation_id, json)`
authority. The generator emits **package-owned typed accessors and
registered operations** — closed CRUD set `get/query/create/update/
delete` (per model, exposure declared; operational records may be
read-only with mutation via commands) — and one data-access kernel
compiled into narrow artifacts (CRUD/data, transaction-command,
projection/BFF, integration). Rev 2's generic-entity MVP default is
superseded: typed, catalog-specific generation IS the model.

Semantics fixed by the POC: three-state update (absent/null/value),
server-owned fields refused on input, optimistic concurrency via a
declared revision field (`not_found` / `concurrency_conflict` / new
revision — no silent last-writer-wins), array envelope with
`per_input` semantics and per-item `request_id`.

## Transactions

CRUD: implicit. Complex atomicity: registered **command** operations —
one explicit transaction inside one component invocation. **A
transaction resource never crosses a wiring edge**; wirings compose
coarse operations. `per_input`: one transaction per outer array item;
cross-item atomicity out of scope.

## Package weld (replaces rev 2's five-part weld)

Each immutable application package records: `verified_schema_state_id` ·
`required_schema_contract` · `required_platform_policy_contract` ·
application SQL corpus identity · source/toolchain provenance. An
additive base schema may satisfy an unchanged overlay's contracts
without rebuilding it.

## Compile-time proves / integration still proves

Compile: SQL parses against the exact effective schema; bind counts and
types; result columns/types/nullability vs generated models; routes
reference real registered operations; contracts and clients compile.
Integration: `current_user` RLS + production privileges; operation
authorization before mutation; `WamnPostgres` type parity; constraints
and data-dependent behavior; locks/optimistic concurrency; rollback and
idempotency; business invariants.

## Work items

1. **`.22.2a` — `WamnPostgres` transport** (unchanged) + the explicit
   **transaction runner** commands require. The only execution path.
2. **`.22.2b` — verifier sibling harness**, now over the full corpus
   (generated + authored `query/*.sql`), on the exact effective schema
   (base + overlay migrations applied); parity refusal; weld emission.
3. **Introspection → IR pipeline** (new): pg_catalog reader over the
   closed supported object set, normalized IR, refused-object
   enforcement (§4.3 of the POC).
4. **CRUD/operation generator** per POC semantics above, emitting
   models, static SQL, accessors, contracts, TS clients, cases.
5. **Shelved (unchanged):** compile-checking for tenant components —
   trigger: a client names it.

## Acceptance

2a: commands run one explicit transaction through the seam under RLS
with per-effect spans. 2b: a broken corpus file fails the build naming
the column; an out-of-vocabulary type refuses at generation; overlay
migration re-runs the check; verified file byte-identical to shipped.
Generator: update preserves all three field states; revision conflict
returns `concurrency_conflict`; server-owned field on input refuses;
a route referencing an unregistered operation fails the build.
