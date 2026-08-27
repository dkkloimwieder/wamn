# Data access — sqlx integration (rev 4.1 patch, future work)

Status: DRAFT rev 2 · 2026-08-26 · amends exe-model §Developer surface ·
evidence: wamn-0h0g.22.13 (receipt 5c464ae) + external review adopted
(two-sibling verification, transport-parity rule, weld set).

## Ruling — one schema, three layers, checked where each build runs

| Layer | Checking | Transport |
|---|---|---|
| Platform-generated artifacts (generated CRUD, catalog-specific modules, base apps) | **Compile-time, two-sibling**: generator emits typed models + exact `.sql` files; a native verifier target runs `query_file_as!` against real `sqlx::Postgres` on an ephemeral DB carrying the applied catalog, `cargo sqlx prepare --check` in CI; the wasm target executes the **same files** via `query_as::<WamnPostgres,_>` | `wamn:postgres` |
| Tenant/developer components | **Runtime-checked** generic sqlx over the custom Database (.22.13: macros type-bind to built-in drivers; masquerade impossible). Drift caught at the gate against the real applied catalog | `wamn:postgres` — guests never hold sockets or credentials |
| Wirer/editor arbitrary queries | Runtime validation + bounded execution only | via `entity` |

**Structural invariant:** the SQL verified through `sqlx::Postgres` == the
SQL executed through `WamnPostgres`, enforced by *one generated file
consumed by both targets* — never two emitted strings compared after.
(Macro expansion type-binds queries to the checking driver; the checked
object cannot ship. The file is the contract; `.sqlx/` is build evidence.)

**Transport-parity rejection:** `wamn:postgres` exposes a bounded type
vocabulary (bool, int, float, text, bytes, numeric, timestamptz, JSON,
UUID, null). Generation **refuses** any checked query whose parameters or
outputs fall outside the runtime mapping.

**Honest guarantee boundary:** the verifier proves SQL validity against
the exact catalog, bind counts/types, and result column names/types/
nullability vs the generated models — nothing more. RLS under production
`current_user`, role privileges, transactional invariants, lock ordering,
and transport encode-parity remain generated integration cases.

**Weld on the released artifact:** catalog identity/hash · generated
model hash · SQL-file-set hash · verification success · runtime
component digest.

## entity — both forms, assigned

Generic `entity` (relation/filter as parameters, runtime-checked) stays
the **MVP default** — one warm pool across all CRUD routes, instant
document-emission generation, correctness proven by auto-written gate
cases against the real catalog. The **catalog-specific compiled entity**
(per-relation typed accessors, two-sibling verified) is the demand-gated
typed tier (`.22.4`) — cheap once 2b exists; trigger: a typed-consumer
pipeline names it.

## Work items

1. **`.22.2a` — runtime transport:** custom sqlx `Database` over
   `wamn:postgres`; generic query/decode, `FromRow`, typed errors; no
   WIT widening. The only execution path for all layers.
2. **`.22.2b` — verifier sibling harness:** generator emits models +
   `.sql` files; native target `query_file_as!` + `prepare --check`
   against ephemeral applied-catalog Postgres; transport-parity
   rejection; weld emission. Red build on schema/SQL mismatch or
   unsupported type.
3. **`.22.4` — catalog-specific compiled entity:** demand-gated, shape
   per above.
4. **Shelved:** compile-checking for tenant components (own checker over
   offline-cache format) — trigger: a client names it.

## Spec deltas (rev 4.1)

Replace the developer-surface data-access paragraph with: *`wamn:postgres`
remains the credential-hiding production boundary. Tenant components and
editor queries use a runtime-checked custom sqlx Database over it — no
macro or offline-cache claim (.22.13, scope: third-party components
only). Platform-generated artifacts are compile-checked per catalog
version via the two-sibling scheme; the released artifact is welded to
catalog identity and query set. Compile-time verification covers SQL
shape and typing; authority, RLS, transactions, and transport parity are
separately proved.* Closure rule: platform artifacts bind `catalog ≥` to
the weld (compile-derived); developer components bind the declared build
fact, enforced at gate.

## Acceptance

2a: entity ops execute through the seam under RLS with per-effect spans.
2b: a broken generated query fails the platform build naming the column;
an out-of-vocabulary type is refused at generation; catalog migration
re-runs the check; verified `.sql` file is byte-identical to the shipped
one. Gate: a developer component with a stale column fails its cases at
authoring, not in prod.
