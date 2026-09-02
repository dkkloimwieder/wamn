# §2a — Effectful-standard-interface admission: the capability registry

Design for owner review · questions **ruled 2026-09-02** · bead `wamn-jrpw`.
**No code has landed**: this document and the spec correction beside it are the
whole change. Nothing in `component-policy`, catalog, admission or connection
vocabulary moves until this is approved.

Import lists measured at `6aed174b` with `wasm-tools component wit`, read
against the host's own plugin worlds rather than authored WIT — because on WASI
the two disagree, and that disagreement is the main finding. Source citations
re-verified at `01ea6382`.

## What is being replaced

Two *implicit* classifiers collapse into one *declared* table:

| deleted | today | why it goes |
|---|---|---|
| `valid_platform_package()` | requires a `wamn:` prefix, rejects `/ @ *` | refuses `wasmcloud:blobstore@0.1.0` twice — namespace **and** `@` |
| `is_effect_package()` | complement: not-WASI and not-`wamn:node` ⇒ effect | true only by accident of a 4-element allowlist |
| `TENANT_WASI_PACKAGES`, `NON_EFFECT_PLATFORM_PACKAGES` | two consts | become rows |

**Registry + shape-check deletion, nothing more.** Exact-version matching needs
no new plumbing: `ComponentImports` already preserves full versioned names
(`wamn:postgres/client@0.1.0`) and `AdmittedComponentEffect.interfaces` already
persists them. Only `import_pkg()` (`component-policy/src/lib.rs:229-232`)
discards the version; policy stops calling it.

## Shape

A `const` table in `wamn-component-policy`, **not** a catalog row:

```rust
pub enum Posture { Ambient, Effect }
pub struct CapabilityRow { package: &'static str, version: &'static str, posture: Posture }
```

Posture is a security classification that must be byte-identical on every host
and must move only through code review plus a ledger row. A catalog table would
let a database edit silently reclassify an effect as ambient.

**The registry does not replace the per-component grant.** Two layers stay: the
registry says what may exist and its posture; `--admit-platform-package`
(`admitted_platform_packages`) still says what *this* component was granted.

## Rows — measured

Two provenance classes, and only one carries the version hazard:

All rows below are the **tenant** path only (see Scope, below the table).

**Class 1 — versions we author** (host plugins in `runtime/wit/world.wit`):

| package | version | posture |
|---|---|---|
| `wamn:node` | `0.1.0` | ambient — inbound seam; the host calls *into* the guest |
| `wamn:postgres` | `0.1.0` | effect |
| `wamn:connection` | `0.1.0` | effect |
| `wasi:logging` | `0.1.0-draft` | ambient — host-implemented by `plugins/wamn_logging.rs` |
| `wasmcloud:blobstore` | `0.1.0` | effect — new, §2b |

**Class 2 — versions we do NOT author** (wasmtime-wasi, rewritten by the virtualizer):

| package | version | posture |
|---|---|---|
| `wasi:io` | `0.2.12` | ambient |
| `wasi:clocks` | `0.2.12` | ambient |
| `wasi:random` | `0.2.12` | ambient |

Eight rows: seven existing, one new. **This is not the spec's list.** §2a names
four WASI + `wamn:postgres` + `wasmcloud:blobstore`; it omits `wamn:node` and
`wamn:connection`, both really granted today (`components/no-std/publish.sh`
grants `wamn:connection` to `http-request`).

Two corrections to my own earlier reporting, both from measurement:
`wasi:random` **is** imported — but only by `http_route` and `materializer`,
which are `wash push`-ed OCI *workloads*, not `wamn-ctl push-component` tenant
admissions, so they never reach `analyze_tenant`. And `wasi:logging` is
imported by no guest at all, yet is host-implemented, so it is an *offered*
capability. Both are registered on the strength of being offered, not imported.

`wamn:jetstream`, `wamn:runner` and `wamn:flow-http-routing` are host plugins
for platform workloads on that same non-tenant path, so they get **no** tenant
row.

**Scope, ruled: the registry governs the tenant path (`analyze_tenant`) only,
and says so deliberately rather than by omission.** `wash push` workloads are
platform-authored and admitted by us, so their imports are already the
platform's own responsibility. Re-convergence trigger for that path: the first
non-platform-authored pushed workload.

**Basis, ruled: register what the host OFFERS, not what a guest imports.**
`wasi:logging` is host-implemented with zero importers; a registry keyed on
imports would drop a live capability the moment its last importer churned, and
would then refuse the next guest to use it. Offered is what the host will
satisfy, and that is the contract.

## The one real hazard: who decides the WASI version

Exact matching makes the version string load-bearing, and for class 2 we do not
author it. Measured on `receiving`:

| artifact | imports |
|---|---|
| authored WIT in-tree | `wasi:*@0.2.12` |
| raw build output | `wasi:*@0.2.9` (+ ten `wasi:cli/*`) |
| **virtualized — what admission sees** | **`wasi:*@0.2.12`** |

The virtualizer *rewrites* the version. Class 2 rows are therefore pinned to
WASI-Virt rev `448f6df8` / adapter sha `28eff8a2…` (ledger row 5), not to our
WIT. Bumping that pin changes the version admission sees and would **silently
refuse every std guest**.

Required mitigation: a conformance test asserts the class-2 rows equal the
versions in the virtualized artifacts, so a virtualizer bump fails loudly at the
gate instead of at admission; and ledger row 5 gains a line saying the registry
rows move with the pin. no_std guests are unaffected — `transform` imports only
`wamn:node/types@0.1.0`.

## Admission flow

1. Key each import on the full `namespace:package@version`.
2. No exact row ⇒ refuse `UnadmittedImport`. **Fail-closed** — an unknown
   package is refused *by absence*, not by a namespace rule.
3. `Posture::Effect` additionally requires the package in this component's
   `admitted_platform_packages` grant.
4. `derive_effects` groups by posture row instead of the complement rule.

## Denial arm

The gate's effect-free-case clause (`scenario-worker/src/store/admission.rs
:782-803`) keys on posture rows. Acceptance: an effect-posture component in a
wiring carrying a non-empty `cases` array refuses with
`EffectfulComponentReached`; effectful components still gate normally
(validation + compatibility) — they are denied only the effect-free case path.

## Migration

Eight rows; delete two functions and two consts; three call sites move
(`component_admission.rs:274`, `component_library.rs:617`, in-crate tests at
`:393`/`:404`). The persisted projection needs no shape change.

## Carried contract question — RATIFIED: hold `wasmcloud:blobstore@0.1.0`

The pinned prior art disagrees with the adopted contract: `examples/blobby`
imports `wasi:blobstore/blobstore@0.2.0-draft`; the spec adopts
`wasmcloud:blobstore@0.1.0`. **Keep the adopted contract** (ruled 2026-09-02):

1. It is what the pinned runtime ships host-side machinery for. Neither has an
   S3 backend upstream, so the draft buys no implementation.
2. Its wasip3 shape carries bodies as native `stream<u8>` and **does not depend
   on `wasi:io`**. The draft does — adopting it would push `wasi:io` resource
   wrappers onto the blob path and widen the ambient surface this registry
   exists to narrow.
3. `0.2.0-draft` is a draft string. Under exact matching, a draft that re-spins
   is a standing refusal.
4. Blobby is prior art for *usage shape*, not for the contract.

Cost, plainly: we diverge from the standard track, so a ledger row lands with
the deviation naming the re-convergence trigger — **the draft stabilizes AND
wasmCloud binds it by default**.

## Rulings (owner, 2026-09-02)

1. **Tenant path only**, stated deliberately in the row table; workload-path
   trigger is the first non-platform-authored pushed workload.
2. **Register on "offered."** A registry keyed on *imported* drops a live
   capability when its last importer churns.
3. **`wamn:node` is ambient.** A third posture for an inbound-only seam is a
   vocabulary expansion with one member and no consumer, which the closed-set
   rule refuses. Trigger, noted and not minted: a **second** inbound-only seam.
4. **`wasmcloud:blobstore@0.1.0` ratified.** The `wasi:io`-widening argument is
   decisive on the registry's own purpose, and the unstable draft version string
   under exact matching seals it. Ledger row lands with the deviation, naming
   the re-convergence trigger: **the draft stabilizes AND wasmCloud binds it by
   default**.
5. The virtualizer-rewrite finding, the required conformance test, and the
   two-provenance-class table split stand as drafted.

## Spec correction, same commit

`wms-prep-spec.md` §2a lists six rows: `wasi:clocks/io/random/logging` ambient,
`wamn:postgres` + `wasmcloud:blobstore` effect. **Seven-plus-one supersedes it.**
The line is corrected in the commit that carries this document, citing the
measurement: the spec omits `wamn:node` and `wamn:connection`, both granted
today, and its basis (imported) is replaced by offered.
