# Charter amendment — the execution plan and the call model

Amends: `docs/scope-reduction-mvp.md` (cut 4: flow calls, the
execution plan, pinning, effect authority; the allowlist line on call
cycles) · owner-directed 2026-08-10, branch `mvp`, tracker `wamn-0h0g`
· standalone; the charter is read through this amendment. Supersedes
the prior draft of this amendment (the pinned Merkle-closure form) in
place.

## Why the chartered model is replaced

Two premises fell, one per stage. **Expansion** was forced by round-7
evidence — at `e5de4356`, effect authority was root-artifact-keyed
(`transitions.rs:116-145`; `connection_http.rs:256,276`), making a
nested callee node unauthorizable. Rounds 8–10 rebuilt authority
around the plan itself, deleting that premise. **Transitive pinning**
(the callee hash inside the caller's identity preimage) then fell on
its own faults: it makes recursion impossible in principle —
`H(A) = hash(bytes containing H(A))` has no construction, and the
published-callee rule cannot bootstrap self- or mutual reference —
and it defeats flows-as-subroutines: a shared callee's fix propagates
to **zero** callers until each revalidates and republishes.

**Owner ruling: callees are late-bound.** `call-flow` is an ordinary
node — like transform, conditional, or an integration node — whose
config names a dependency (`flow-id`). Resolution happens against the
environment's **deployed** set, on the platform's existing precedent
for late-bound dependencies: connection bindings are
environment-owned, publication checks are point-in-time, later
changes are governed by the dependency's own controls rather than
retroactive invalidation, and evidence records what was *exercised*
as facts about the test, not constraints on the future.

## The execution plan

**One immutable, content-addressed plan per resolved flow
executable** — validated drafts included; a published release member
and a validated draft each reference their plan hash; identical bytes
deduplicate naturally. A plan contains **only that flow's own
structure**: entry instruction · nodes (local node id · source node
id · type · config · effect policy `pure | effectful` · resolved
connection requirement) · edges with source/destination ports ·
fan-out ordinals · root terminal behavior · the compiled input-schema
guard · recorded callability (below) · source map. Header:
`ExecutionRuntimeRevision { flowrunner_component_digest,
effect_provider_revision, host_effect_contract_version }` +
`root_artifact_hash`. A `call-flow` instruction is
`{ site, flow_id }` — **no callee hash is stored in the plan.**

**Identity is byte identity — hash the data — and the compiler is a
function.** The plan is the exact bytes the validator emitted;
`execution_bundle_hash = sha256(exact bytes)`; stored once. No
canonicalization layer, no preimage specification — everything in
the serialized plan is in the hash automatically. But
nondeterministic bytes are **not** harmless (they would break
`validate`'s contract idempotency, split replicas, and let a rolling
compiler mint phantom identities), so deterministic encoding is a
contract of the plan compiler: **same plan format version + same
`plan_compiler_revision` + same semantic inputs = exactly the same
bytes** — sorted collections, no serializer-dependent ordering,
golden byte vectors, one cross-process deterministic-rebuild test.
`plan_compiler_revision` joins the plan header; a compiler or
serializer change is a named revision requiring revalidation, never
silent drift. The bytes are the serializer's JSON, cast to `jsonb`
where querying wants structure; the exact bytes remain the sole
identity source. `catalog.execution_bundles` is **immutable and
append-only in MVP; no plan garbage collection is performed**; the
tenant-scoped PK, hash CHECK, and FKs from drafts, evidence, and runs
stand.

**What the hash still buys** (and it is all own-plan): the weld
between the test report and the exact executable bytes; the content
address and dedup; claim-time integrity; the runtime-revision
binding. **What it no longer does:** encode callees. Identity is
per-flow; composition is resolved at execution.

## Release-bound resolution at claim — one immutable map per run

Frames never resolve names, and resolution never reads a moving
head. Every admitted run pins an immutable `catalog_id +
catalog_version`; **resolution walks the run's pinned release**, so
the executable is a function of the release, never of queue timing,
and callee plans meet connection bindings keyed by the same
`(catalog, version, artifact, requirement)` identity. Activating a
later catalog release affects later admissions, never an admitted or
queued run. The owner's goal survives exactly: publish a catalog
version carrying the fixed callee, and every **newly admitted**
caller run uses it with zero caller revalidation — the unit of
change is the catalog release.

**The root claim transaction, atomically:** lock and classify the
queue row (the single-shot reclaim classifier, unchanged) · resolve
the transitive reachable *name set* from the root plan against the
pinned release (finite; recursion means a name re-enters the set) ·
verify every resolved plan's bytes, the **single** environment
execution-runtime revision, each callee's callable contract, and
every reachable connection requirement's binding · **insert the
immutable resolution rows** · acquire the execution claim. The map
is a dedicated immutable relation — not `invocation_context`
(size-capped JSON is the wrong home for a load-bearing identity):

```text
run_flow_resolutions {
  tenant_id, run_id, flow_id,
  execution_bundle_hash, source_artifact_hash,
  PRIMARY KEY (tenant_id, run_id, flow_id)
}
```

A claim retry finds the same complete map or refuses. An
unresolvable name, hash-invalid bytes, a foreign revision, an
incompatible contract, or an unbound requirement **refuses at
claim**, typed, before any guest execution. Frames perform in-memory
lookups against the map; a callee republishing mid-run is invisible
to that run. The plan cache is bounded, keyed
`(tenant_id, execution_bundle_hash)`.

**Draft/test bootstrap rule (narrowed):** during validate,
draft-run, and test-set-run, a reference to the flow under test —
and only that reference — resolves to the **candidate** plan; every
other name resolves against the pinned release. This supports
self-recursion and cycles through already-published partners.
**First publication of a brand-new mutually recursive group is
outside MVP** — every non-root participant must already exist in the
pinned catalog release (a multi-draft atomic publish is a separate
authoring subsystem). **Test-set consistency:** one report resolves
one world — same catalog version, same candidate override, one
report-level resolution map recorded (or its hash), and every case
run must match it.

## Execution: frames under one root run

The flowrunner keeps an in-memory frame stack. At `call-flow`: look
up the callee plan in the run's snapshot → run the **call-enter
guard** (live payload against the callee's compiled input schema;
failure = a typed `invalid-input` node error at the call site) → push
and execute → the callee's terminal `respond` **body** becomes the
site's `main` output → pop; an unhandled callee failure is a normal
node error and may follow the site's `error` edge. A nested `respond`
never releases the root caller. **Single-shot is untouched**: one
root run, one claim; frames are memory; every write-ahead intent is a
root-run intent; a crash after any callee effect makes the root run
`effect-uncertain` under the unchanged classifier and operator
terminalize. No child run, queue row, parent wait, actor mode,
lineage, cancellation, or independent recovery exists.

**Recursion and loops are supported.** Self- and mutual reference are
ordinary calls. Termination is runtime-enforced: a **maximum call
depth** per root run and a **total-dispatched-node budget**;
exhaustion is a typed failure (`depth-budget` / `dispatch-budget`)
failing the run. No static **termination** analysis ships in MVP
(claim necessarily performs static call-graph *reachability* to build
the snapshot). A bounded `for-each` node is outside MVP,
demand-gated.

**Node identity: dynamic frames for uniqueness, stable selectors
for authors.** A call-site chain grows per recursion depth, so it
cannot be a static identity. Durable uniqueness is the frame:
`frame_id` (root-run-local, monotonic) · `parent_frame_id` ·
`call_site_id` · `current_plan_hash` · `local_node_id` ·
`occurrence`; a node fact or attempt is keyed
`(run_id, frame_id, local_node_id, occurrence)`. The
**author-facing assertion selector is `(flow_id, node_id)`** —
named-node assertions read: *every observed frame and occurrence of
this flow/node pair must match* — bounded, and simpler for authors
than paths. Call-site-specific selectors are a future explicit
addition. The node-id charset rule stands.

**Effect authority uses the current frame directly.** Every
dispatched effect carries trusted execution facts: root plan hash
(the run identity and anchor, `runs.execution_bundle_hash` non-null)
· **current plan hash** · `frame_id` · local node id · source
artifact hash · requirement name. Authority verifies: the run's
resolution map contains the current plan hash · the current plan
contains the node · the frame descends from the root frame · the
attempt identity matches `(frame, node, occurrence)`; then resolves
that plan's source artifact + requirement → environment binding →
active generation — no per-effect graph walk. Callee effects authorize against the callee's identity
while executing under the root run, principal, trace, deadline, and
budgets. The attempt ledger is self-describing.

**The call contract is intrinsic — recorded data, not a flag.** At
its own validation, every callable plan records a
`CallableContract { version, input_schema_hash, return_contract,
effect_ceiling }` alongside the eligibility rules (request-entry
flow; every successful path reaches a `respond`; boundary `respond`s
have zero outgoing edges; no success-by-frontier-exhaustion). MVP
values, stated honestly: `return_contract = untyped-json-body` (the
charter's respond-body-only limitation, as data — a successful call
returns arbitrary JSON, and output shape may change when a callee
republishes) and `effect_ceiling = effectful` — **every `call-flow`
classifies as effectful**, because a late-bound callee may become
effectful after the caller deployed; by cut 4's existing rule, any
synchronous attachment whose flow contains a `call-flow` therefore
requires an idempotency key. A caller's validation resolves each
name against its pinned view and refuses on missing callability or
contract incompatibility — point-in-time; the runtime call-enter
guard covers the **input** direction authoritatively (drift becomes
a typed `invalid-input` at the site), and claim's preflight
re-verifies contract compatibility for the whole reachable set.
Output-schema compatibility is a future contract version, not an MVP
promise.

## Publication and the gate claim

Publication verifies: the flow's **own** plan bytes present,
hash-valid, and carrying the environment's runtime revision · every
named callee **currently resolves** in the target release with
recorded callability · every connection requirement bound —
point-in-time, exactly like connection generations. The evidence row
records the **`tested_resolution_map`** (`flow_id → plan_hash`, the
report's map) beside the exercised connection generations, and the
**`deployed_resolution_map`** as resolved at publication — the split
that makes the narrowed claim auditable instead of implied.
Retention: release → evidence → own bundle; recorded hashes stay
resolvable because `execution_bundles` is append-only.

**The gate claim, narrowed and stated honestly:** the gate proves
*this flow's own executable*, exercised against the callee versions
recorded in evidence. A callee that republishes afterward changes
caller behavior **without caller retest — by design**: the callee's
own unconditional gate governs it, the call-enter guard converts
interface drift into a typed refusal at the site, and evidence plus
the per-run resolution map make any historical execution exactly
reconstructable.

## Delta against the chartered text

**Deleted:** the recursive expander · callee inlining and
namespacing rewrites · the canonical-JSON / canonicalized-preimage
requirement (identity is byte identity) · plan-inlined synthetic call instructions (the
enter guard is frame-entry behavior; return is frame pop) · cycle
refusal · the expanded-node bound · callee pinning, the call-map
preimage, and closure-DAG publication walking · the
"callee drift mints a new caller identity" semantics.
**Added:** the frame stack (memory only) · release-bound claim-time
resolution with the immutable `run_flow_resolutions` relation · the
`CallableContract` record (untyped-json-body; effectful ceiling —
call-flow ⇒ idempotency key) · frame identity
`(frame_id, parent, site, plan, node, occurrence)` with the
`(flow_id, node_id)` assertion selector · `plan_compiler_revision` +
the deterministic-encoding contract · tested/deployed resolution
maps in evidence · the two runtime budgets · self-describing effect
attempts · the narrowed bootstrap rule (new mutually recursive
groups outside MVP). **Owner-local proofs update:** frame
push/pop success- and failure-conversion · budget-exhaustion typed
failures · a recursion case terminating by budget · claim-time
refusals (unresolvable name, hash-invalid bytes, foreign revision) ·
snapshot atomicity across a mid-run callee republish · the bootstrap
self-resolution case · the guard's interface-drift case. **The M0
gate is unchanged** — the one integrated check reads *caller → framed
callee → one real Postgres effect → result on the caller's `main`,
all facts under one root run*; the 16-check count stands.

**Owner decisions recorded here:** byte identity over canonicalized
identity (hash the emitted bytes; no preimage layer; the compiler is
a deterministic function) · late binding over pinning, **release-
bound** — the catalog release is the unit of change, never the claim
instant (ratified above) · recursion in — the reviewer's retained cycle
refusal patched the abandoned identity scheme and is rejected with
it · the effect-retry cut stands as closed (`wamn-0h0g.13.2`: one
dispatch per effectful occurrence, no stable-key alternative) — the
landed stable-key descriptor support is deletion inventory, not an
open choice.

**Read-as:** wherever the charter says canonical plan bytes or
enumerates a hash preimage, read: the exact serialized plan bytes.
Wherever the charter's cut 4 describes expansion,
inlined callee structure, synthetic plan instructions, cycle
refusal, the expanded-node bound, callee pinning, or
drift-invalidation, read this amendment; the allowlist's "reject
recursive call cycles" reads "recursion bounded by the runtime depth
and dispatch budgets." The `call-flow { flow-id }` public shape, the
`respond`-body-only limitation, the effect-uncertain literal, caller
completion semantics, and the idempotency-key requirement stand
unmodified.
