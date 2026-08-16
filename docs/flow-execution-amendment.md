# Charter amendment — the execution plan and the call model

Amends: `docs/scope-reduction-mvp.md` (cut 4: flow calls, the
execution plan, pinning, effect authority; the allowlist line on call
cycles) · owner-directed 2026-08-10, branch `mvp`, tracker `wamn-0h0g`
· standalone; the charter is read through this amendment. Supersedes
the prior draft of this amendment (the pinned Merkle-closure form) in
place.

> **Deployment-simplification amendment (owner-ratified 2026-08-16 by
> `wamn-0h0g.13.43`; the test-contract clauses by `wamn-0h0g.13.44`).**
> Read this document through `docs/deployment-simplification-spec.md`.
> **Runs are never version-pinned**: a run executes under the release
> its claiming pod carries and records `(release version, manifest
> digest)` write-once at claim, and resolution is a pure read of that
> pod's mounted release manifest. The *Release-bound resolution at
> claim* section below is superseded whole, and the clauses elsewhere
> that depend on it are either marked where they appear or re-read
> inside that section's own marker — the two "snapshot" clauses in
> *Execution: frames under one root run* are re-read there, not marked
> in place. Every unmarked section stands: the execution plan and its
> header, byte identity and the deterministic compiler, append-only
> `execution_bundles` (explicitly affirmed by ruling 2), the frame
> stack, recursion and the two budgets, frame identity and the
> `(flow_id, node_id)` selector, the intrinsic call contract, and
> effect authority's verification set — which changes only in the
> wording of its map lookup.

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

> **Superseded — deployment simplification (`wamn-0h0g.13.43`;
> `docs/deployment-simplification-spec.md`, "Deleted by this ruling"
> and "Version semantics (no pinning)").** This section's ruling —
> every admitted run pins `catalog_id + catalog_version`, and
> resolution walks the run's pinned release — is deleted
> (`wamn-0h0g.15.10`, `.15.11`). **Runs execute under the current
> release of the claiming pod**, the standard job-queue semantic;
> rollout overlap behaves as it does for any HTTP service behind a
> load balancer, and drain completes it. Deleted with it: the
> `run_flow_resolutions` relation and its insert, the five-step claim,
> every claim-time resolution and verification leg (name resolution,
> callable-contract re-check, per-requirement binding check, and the
> single-environment execution-runtime-revision check — ruling 4 ships
> no revision check at all), and the per-run resolution map as an
> audit object. **What replaces it:** the claim is lock → classify →
> lease — the single-shot reclaim classifier is unchanged — plus a
> write-once record of `(release version, manifest digest)` taken from
> the claiming pod's identity under the existing immutability trigger;
> resolution is a pure read of that pod's mounted release manifest,
> whose call-edge adjacency yields the transitive set
> (`wamn-0h0g.15.13`); plan bytes fetch by digest from OCI, verify at
> transfer, and cache for the process lifetime (ruling 3). The plan
> cache's **bound** is not ruled on and does not die with this
> section: ruling 3 governs invalidation, not capacity, so the cache
> stays bounded and keyed `(tenant_id, execution_bundle_hash)` — the
> same bounded process-memory cache stands unmarked at
> `docs/plane-amendment.md:259-260`. The immutability the pinned map
> bought is bought instead by construction: the manifest deploys as an
> **immutable ConfigMap named by its digest**, referenced by that name
> in the pod template, so manifest and image are atomic per pod and a
> callee republishing mid-run is still invisible to a running run
> (ruling 4). Refusals
> move rather than vanish — unbound connection requirements and
> unfetchable or hash-mismatched plans are gated at **readiness**; a
> hash mismatch on fetch remains an integrity refusal that never
> executes; a `(flow, plan-hash)` absent from the run's recorded
> manifest is caught by effect authority. Breaking input contracts are
> author versioning events (publish a new attachment, migrate callers,
> tombstone the old), not a pinning knob: there is no agreement knob
> and no new refusal category. The audit chain is run → recorded
> version → manifest digest → plan hashes → bytes. Two terms defined
> here and used later read differently: **"the run's snapshot"** (in
> *Execution: frames under one root run*) is the pod's mounted
> manifest and its plan cache, not a per-run map; and "claim
> necessarily performs static call-graph *reachability* to build the
> snapshot" reads — the manifest's call-edge adjacency, materialized
> once at mint, supplies that reachability, and claim performs no walk.
> **The draft/test bootstrap rule survives as data — see the next
> marker.**

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

> **Retained as data, one clause deleted — deployment simplification
> (`wamn-0h0g.13.43`, ruling 1; report-level map-consistency
> *checking* per "Deleted by this ruling"; cases source per
> `wamn-0h0g.13.44`).** The
> narrowed bootstrap rule stands, re-expressed as data:
> `test-set-run` and `draft-run` materialize the **candidate
> manifest** (released set + candidate overlay) as a scratch
> ConfigMap, a per-report **Job** whose pods mount it executes the
> cases, and claims are targeted at that scratch claimant through the
> landed `wamn-0h0g.5.9` placement seam (`execution_target_id`). No
> pin exception exists — the candidate *is* the claiming pod's
> release — and post-publish testing is rejected, since it inverts the
> gate ordering. Read "the pinned release" as "the released set inside
> the candidate manifest"; the first-publication limit on brand-new
> mutually recursive groups is unchanged. **Deleted:** the
> test-set-consistency clause's closing requirement — "**and every
> case run must match it**" — one report resolves one world *by
> construction*, because every case mounts the same immutable
> candidate ConfigMap, so there is nothing left to re-aggregate a
> per-case map against. **Retained:** the recorded **report-level
> resolution map and its hash**. What dies is map-consistency
> *checking*, not the map. The report-level map is the source of the
> `tested_resolution_map` the publish gate consumes
> (`docs/deployment-simplification-spec.md:34`), and the spec's own
> Tier B entry retains that evidence column in the same breath as it
> deletes the check (`:152-154`). The landed shape agrees:
> `deploy/sql/authoring-tests.sql:190-191` names the report's map "the
> exact `flow_id -> execution_bundle_hash` object consumed later as
> `tested_resolution_map`", and `:199-201` keeps `resolution_map` and
> `resolution_map_hash` `NOT NULL` under their self-consistency hash
> `CHECK`. Deleted in Tier B (`wamn-0h0g.15.7`, landed `1dbfd097`) is
> only the checking machinery: the `deploy/sql/authoring-tests.sql`
> trigger legs that re-aggregated `run_flow_resolutions` against the
> recorded map, and their scenario-worker caller. Under
> `wamn-0h0g.13.44` the cases come from the draft's own `cases` array,
> not a separate test-set store (`wamn-0h0g.15.27`).

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
ordinary calls. Termination is runtime-enforced by two named platform
constants: `MAX_CALL_DEPTH = 64` (root depth zero, so 64 active
callees are admitted) and `DEFAULT_ROOT_DISPATCH_BUDGET = 10_000`
across the whole root run. The dispatch counter is debited once for
every emitted `Step::Dispatch`, including ordinary loop re-dispatches
and `call-flow`; dispatch 10,001 fails `dispatch-budget`. For a call,
the debit precedes the callee input guard, which precedes the depth
check and frame-id allocation; the 65th callee therefore fails
`depth-budget` without consuming a frame id. Both resource failures
are terminal, attributed to the active caller-frame node occurrence,
and never follow a call-site error edge. Per-environment configuration
of either value is demand-gated. No static **termination** analysis
ships in MVP (claim necessarily performs static call-graph
*reachability* to build the snapshot). A bounded `for-each` node is
outside MVP, demand-gated.

**Node identity: dynamic frames for uniqueness, stable selectors
for authors.** A call-site chain grows per recursion depth, so it
cannot be a static identity. Durable uniqueness for a recorded node
fact or attempt comes from the executing frame:
`frame_id` (root-run-local, monotonic) · `parent_frame_id` ·
`call_site_id` · `current_plan_hash` · `local_node_id` ·
`occurrence`; a node fact or attempt is keyed
`(run_id, frame_id, local_node_id, occurrence)`. The
**author-facing assertion selector is `(flow_id, node_id)`** —
named-node assertions read: *every observed frame and occurrence of
this flow/node pair must match* — bounded, and simpler for authors
than paths. Call-site-specific selectors are a future explicit
addition. The node-id charset rule stands.

> **One clause superseded — deployment simplification
> (`wamn-0h0g.13.43`).** Effect authority's verification set is
> untouched **except the map lookup**: for "the run's resolution map
> contains the current plan hash" read "**the release manifest
> recorded on the run at claim contains `(flow, plan-hash)`**" — the
> rest of the chain (plan contains node → immutable attempt matches
> `(frame, node, occurrence)` and the trusted effect facts → source
> artifact + requirement → binding → active generation) is unchanged,
> as are the attempt ledger's self-description and the absence of a
> `run_frames` relation. The root plan hash remains the run identity
> anchor carried on every dispatched effect, but it is no longer
> pinned at admission: the admission-time bundle pin moves to the
> claim-time write-once recording of `(release version, manifest
> digest)` (`wamn-0h0g.15.11`); the exact column shape is that lane's
> work, not this amendment's.

**Effect authority uses the current frame directly.** Every
dispatched effect carries trusted execution facts: root plan hash
(the run identity and anchor, `runs.execution_bundle_hash` non-null)
· **current plan hash** · `frame_id` · local node id · source
artifact hash · requirement name · `occurrence`. Authority verifies:
the run's resolution map contains the current plan hash · the current plan
contains the node · the immutable attempt row matches the selected
`(frame, node, occurrence)` and trusted effect facts; then resolves
that plan's source artifact + requirement → environment binding →
active generation — no per-effect graph walk. Callee effects
authorize against the callee's identity while executing under the
root run, principal, trace, deadline, and budgets. The attempt ledger
is self-describing. Each immutable attempt row is runtime-attested
ancestry for that effect's link: the trusted interpreter mints the
frame and its parent link into the existing pre-dispatch write-ahead
row. Links through pure intermediate frames are attested by descendant
attempt records; the pure frames are not independently row-backed.
There is no `run_frames` relation or other frame registry. Full descent
from the root is therefore not a separate authorization check.

> **One clause superseded — deployment simplification
> (`wamn-0h0g.13.43`).** "Claim's preflight re-verifies contract
> compatibility for the whole reachable set" dies with the five-step
> claim; there is no preflight (`wamn-0h0g.15.10`). Callable contracts
> are recorded in the plan and projected into the release manifest, so
> compatibility is checked at the callee's own validation and at the
> publish gate — point-in-time, unchanged — and the runtime
> call-enter guard remains the authoritative input-direction check,
> converting drift into a typed `invalid-input` at the site. The rest
> of the call contract stands: intrinsic recording,
> `return_contract = untyped-json-body`,
> `effect_ceiling = effectful`, and call-flow ⇒ idempotency key.

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

> **Partly superseded — deployment simplification (`wamn-0h0g.13.43`,
> ruling 5).** The publish gate itself is unchanged and remains the
> gate; publication still verifies own plan bytes, current callee
> resolution with recorded callability, and bound requirements,
> point-in-time. Two clauses die. (1) The **`deployed_resolution_map`**
> is dropped — here from the evidence row and, in
> `docs/plane-amendment.md`, from the deployment attestation; it is
> derivable (digest → manifest → map). `tested_resolution_map` is
> **retained** in release evidence, as are the attestation's six-part
> coordinate, `deployed_manifest_hash`, and `attested_at`
> (`wamn-0h0g.15.8`). (2) "Evidence plus the per-run resolution map
> make any historical execution exactly reconstructable" reads:
> evidence plus the `(release version, manifest digest)` recorded on
> the run at claim — run → recorded version → manifest digest → plan
> hashes → bytes, every link content-addressed and immutable. The
> narrowed gate claim, the callee's own unconditional gate, and the
> call-enter guard are unchanged, and `execution_bundles` stays
> append-only (ruling 2) so recorded hashes stay resolvable.

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

> **Partly superseded — deployment simplification (`wamn-0h0g.13.43`;
> test-contract clauses `wamn-0h0g.13.44`).** The **Deleted** list
> stands in full; nothing it retired comes back. Four entries below do
> not stand. **Added:** "release-bound claim-time resolution with the
> immutable `run_flow_resolutions` relation" is deleted — read: the
> claim records `(release version, manifest digest)` write-once and
> resolves by reading the pod's mounted manifest — and
> "tested/deployed resolution maps in evidence" loses its deployed
> half (ruling 5). Everything else added here stands: the frame stack,
> the `CallableContract` record, frame identity with the
> `(flow_id, node_id)` selector, `plan_compiler_revision` and the
> deterministic-encoding contract, the two runtime budgets,
> self-describing effect attempts, and the narrowed bootstrap rule (as
> data, per ruling 1). **Owner-local proofs:** the claim-time refusal
> proofs (unresolvable name, hash-invalid bytes, foreign revision)
> retire with the five-step claim — hash-invalid bytes survive as an
> integrity refusal at fetch, unbound requirements move to the
> readiness gate, and the revision check is deleted outright (ruling
> 4); "snapshot atomicity across a mid-run callee republish" is now
> proved by construction (the pod's manifest ConfigMap is immutable
> and digest-named), not by a per-run map. The remaining proofs and
> the unchanged M0 gate with its 16-check count stand. **Owner
> decisions:** "late binding over pinning, **release-bound** — the
> catalog release is the unit of change, never the claim instant" is
> superseded — late binding stands, but the unit is the release the
> **claiming pod** carries; no run is pinned and no version-pinning
> knob exists. **Read-as:** those clauses stand and gain one hop —
> wherever the charter points at this amendment for expansion,
> inlining, synthetic instructions, cycle refusal, the expanded-node
> bound, callee pinning, or drift-invalidation, read this amendment
> **as amended here** by `docs/deployment-simplification-spec.md`
> (the charter's own read-through lands with `wamn-0h0g.15.24`). The
> `call-flow { flow-id }` public shape, the respond-body-only
> limitation, the `effect-uncertain` literal, caller completion
> semantics, and the idempotency-key requirement stand unmodified.

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
