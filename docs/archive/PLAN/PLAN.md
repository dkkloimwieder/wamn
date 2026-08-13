---
status: active
genre:  plan
date:   2026-07-27
verified-against: 7ea1fb2
verification-date: 2026-08-06
horizon: post-POC (assumes FLOW-SPEC rev18 + POC-PLAN r6 land)
---

# Wamn — the plan

An opinionated low-code platform for industrial clients: visual dataflows, built-in
Postgres, schema designer, generated APIs, hosted frontends — WASI components on
wasmCloud atop Kubernetes.

## What this document is, and is not

**Is:** the order of work *after* the callable-flow slice lands, and — under each item —
what is already decided that constrains it, what must still be decided before it can
finish, how we will know it succeeded, how we will know it failed, and what we would do
instead.

**Is not:** a specification, and not a description of the system. Invariants live in code
and gates, which can fail; a plan that restates them makes a second copy with no test
attached. Completion status lives in `bd` and git. **Deliberately selective, not small** —
the test is whether a claim earns its place, not how many there are.

**This document is the authoritative roadmap and decision map.** A separate normative
document is created only when a protocol or state machine cannot be safely implemented from
this plan — the way `FLOW-SPEC.md` and `POC-PLAN.md` serve the callable-flow work. **Any
durable decision folds back into `PLAN.md` immediately**; the separate document carries
implementation-period detail only and is deleted when its work closes.

**Status labels**, used where a passage could otherwise be misread:

| | Meaning |
|---|---|
| **Decided** | settled; changing it means revisiting this plan |
| **Direction** | the intended answer, gated on named evidence |
| **Candidate** | a plausible shape, recorded so the option is not re-derived |
| **Example** | illustrative only; nothing depends on the specifics |

**Detail is guidance, not specification.** Where an item carries a table of settings, a
worked example, or a comparison of mechanisms, it is there to make a *direction* legible —
why this way and not that. **Numbers and examples are illustrative unless explicitly marked
measured.** Anything that hardens into a contract belongs in the item-local design document
written when the item starts, and dies with it.

**Entry gate, not an all-or-nothing premise.** Items 1 and 2 of the previous plan —
callable flows and child flows — are **the entry gate**: typed entries, immutable artifacts, releases, catalog
heads, the admission ledger, fenced transitions, run context, the attempt protocol, the
invocation service, generic ingress, and `invoke-flow`, all proven by the POC ladder from a
reprovisioned database. Every item below starts from that.

**If the entry gate changes materially, the dependency graph and the affected items are
re-baselined before work proceeds — independent decisions and continuous obligations remain
valid.** Gate integrity, the security closes, identity, the connection direction, node
distribution, and the authoring-product goal do not become invalid because one invocation
feature slipped.

**On the `D` numbers.** Owner decisions, recorded with attribution, dates and rejected
alternatives in `docs/archive/platform-plan.md`, **retained as the archive of record**. Each line
here is a one-line restatement placed under the item that spends it; the number points back
to the full row *and to the alternative that was rejected*. Do not treat a restatement as
the decision.

**Verification.** Claims below were checked against code, SQL, and gates at `7ea1fb2`.
**Re-verify before use** — the previous revision of this document was pinned 21 commits
back and four of its factual claims had already gone false, including two of the three
supports under its largest risk. A stale tip line is not metadata; it is an expiry date.

---

## Summary — what each item owns

The durable part of this document. Reversibility drives sequence: **how expensive is it to
change this after data exists?**

| # | Item | Owns | Depends on | Reversibility |
|---|---|---|---|---|
| 0 | Continuous obligations | backlog, gate integrity, fork sync | — | — |
| 1 | Payload durability & blob boundary | what is durable, where bytes live | entry gate | **irreversible** — re-keys durable state |
| 2A | Execution-bundle specialization | whether capability narrowing is structural | entry gate | **expensive** — rework, no migration |
| 2B | Connections | env/artifact boundary for externals | entry gate (parallel to 2A) | **irreversible** — artifact contract |
| 2C | Node authoring & distribution | the catalogue's growth path | entry gate; **1 only for the bulk-capable node contract and first bulk connector** | expensive |
| 2D | Custom-node composition, patchability | hop vs compose; fleet maintenance | 2A, 2C | reversible |
| 3 | Measurement gate | the R6 verdict, deferred constants | 1, 2A | — (a gate) |
| 4 | Security structural closes | trust-level gap in the shipped surface | — | **irreversible** once exposed |
| 5 | Identity & access | who a principal is, at which tier | entry gate | **irreversible** — key encoding |
| 6A | Minimum authoring loop | frontend-neutral edit → validate → draft-run → observe | 2A; 5 before client exposure (+2B for the outbound journey) | expensive |
| 6B | Complete studio | canvas, palette, exposure UX | 6A | reversible |
| 7 | Release normalization | what a release contains, its lifecycle | entry gate | expensive |
| 8 | Event plane completion | registration exposure, convergence | 7; **external precondition:** an org bootstrapped by platform administration | expensive |
| 9A | Generated API + SDK | what the client's users consume | 5; **7 if the SDK covers callable contracts** (output/error schemas are item 7) — else scope 9A's first cut to the fields available before them | expensive |
| 9B | Frontend hosting | serving their SPA | 9A | reversible |
| 10 | Operator journey & project lifecycle | provisioning as a control loop, templates | 5, 7, **3's fanout characterization**; a *useful* starter vertical also needs 2B (connection requirements) and 2C (its node bundle) | expensive |
| 11 | Production readiness | posture, drills, quotas at scale | 3, 10 | reversible |

---

## A possible sequence

**Provisional by construction.** Several orderings below cannot be settled until something
is built, and the plan should not pretend otherwise.

**Forced by architecture** (these are not preferences):

- Item 3 cannot precede the model it measures — 1 and 2A.
- Item 5 blocks every client-facing surface; its key encoding must settle before **retained
  client data, externally meaningful identities, or self-service projects** accumulate. (Not
  "before durable data" — the entry-gate POC already creates durable run and artifact state.)
- Item 2A precedes 3 and 6A: it **changes the execution model**, so measuring or building
  a loop against the interpreter and then switching is rework where H1 makes rework
  expensive. It develops against `ctl` and fixtures — a studio is convenience, not a
  dependency.
- The completed **v2.6.1 fork upgrade precedes 1 and 2A** (0.3) — both build against the runtime.
- Item 8 needs 7's registration versioning.
- Items 1 and 2A are **largely parallel**: checkpointing is host-side (`run-state`),
  composition is guest-side. Only 2C's bulk connectors need payload handles.

**But they share one canonical resolved-node contract.** Parallel work meets at node
identity — item 1 consumes a node's `pure | effectful` policy, while 2A keys execution
bundles on interface versions, digests, and capability classes. If each defines its own
canonical descriptor, effect policy could change without invalidating a composition, or the
reverse.

> One resolved-node contract carries the strict interface identity, declared ports,
> capability classes, `pure | effectful` policy, executable/component identity, and a
> contract version. **Artifact validation, artifact identity, effect planning, and
> execution-bundle identity all consume that same resolution; runtime node-type tables do
> not supply missing semantics.**

It holds **environment-independent** facts only: endpoint, credential, and policy facts
belong to the connection instance, never to this contract or to bundle identity — see 2B.
No struct, format, or package is prescribed;
this names the owner-level source of truth before parallel work starts. (The contract version exists for the same reason
`admission_context_version` does: artifacts pin a *resolved* contract, so the shape they
resolved under must remain recoverable.)

**Joint milestone:** one standard node and one custom node resolve through the same
canonical contract. Changing any effect-policy-, interface-, capability-, or
executable-relevant field invalidates every artifact and bundle identity that depends on it.

**2A and 2B share a capability-bearing integration gate.** 2A's decisive experiment includes
a capability-bearing node, but 2B owns the connection ABI it must use. A plug built against
the old shape — *node imports generic HTTP and constructs an absolute URL* — is not evidence
for the intended one — *node declares a typed connection requirement, host supplies the
resolved connection*. Those are different component interfaces and possibly different
composition topologies.

> The **pure-node** packaging experiment proceeds independently. 2A's **capability-bearing**
> composition result and its final economics use 2B's minimum typed-connection WIT and host
> adapter. A material change to that interface invalidates the corresponding 2A evidence.

This does not serialize the two items; it stops 2A declaring success against an ABI 2B then
replaces. Consequently the canonical resolved-node contract carries the **portable connection
requirement type and contract identity**, and excludes environment instance data and
policy facts.

**Preference, not necessity:** where item 4 sits in a linear list (it is parallel
throughout, gated only by exposure), when 2B lands relative to the first loop, whether 9A
precedes 6B.

**Sequencing decisions that cannot be made until something is built** — each reshapes what
follows:

| Decision | Made during | If it goes the other way |
|---|---|---|
| Does composition prove viable — cache hit rate, cold start, memory? | 2A | least privilege stays a runtime check; 6A builds against the interpreter |
| Does R6 survive measurement? | 3 | the execution model changes and items 1, 2A churn |
| Does the studio consume the generated API? | 6A | 9A moves ahead of 6B |
| Do custom nodes compose in, or keep D7's hop? | 2D | the supply chain signs compositions, not components |
| Does the E1–E11 roadmap survive triage? | 0.1 | items 7–10 change shape |

**A plausible ordering, then:**

```
continuous:  0 · backlog · gate integrity · fork sync
             4 · security closes  (parallel throughout; gates exposure, not work)

execution:   1 · payload/checkpoint  ∥  2A · composition  ∥  2B · connections
             3 · measurement verdict  (after 1 + 2A)

product:     5 · identity  →  6A · minimum authoring loop
             (begins after 2A, not after the whole component ecosystem;
              2B needed only for 6A's outbound-integration journey)

platform:    2C · node distribution · 7 · releases · 8 · events
             9A · API/SDK · 10 · provisioning · 6B · studio · 9B · hosting
             11 · production posture
```

**Ordering principle.** *Irreversible architecture constrains sequence; evidence determines
how much of each layer to build.* The first clause is why 1, 2A, 2B, and 5 come early and
why market pull does not reorder them. The second is why the reversible layers — catalogue
breadth, studio depth, hosting — are sized by what pilots actually need rather than built
out speculatively.

**This ordering will iterate.** Items 1 and 2A both change the execution model and may
invalidate assumptions in items already written. Expect to revisit earlier items rather
than treat them as closed doors.

**Next wave (settled 2026-08-04): the iteration loop.** With the fork base
complete and items 1, 2A, and 2B at their v1 floors, the sequence turns to
the loop every later item consumes: item 6A's minimum authoring loop (the
draft model and its retention, draft-safe connections, draft-workspace
scope, public versioned authoring projections), item 7's release normalization,
and the flow-testing surface (client-driven test execution, stored suites, publish
gates, ephemeral per-run schema isolation). The parked durable-execution
tail — semantic strengthening and its revalidation economics, the payload
store and threshold campaign, re-attempt and compensation verbs, declarative
failure policy, and the controlled-Replay protocol — resumes only on its
recorded reactivation condition in item 1.

**Exit gate for the wave:** through the authoring surface from a client, an author edit
reaches a draft execution without minting a release and a stored suite runs before publish,
with edit→run latency measured and recorded. The reference demonstration is deliberately
headless: CI closes edit → validate → draft-run → suite-run → publish from a checkout
with zero frontend code present. That proves client decoupling; it is not a weaker substitute
for an editor, because **editor is a client role rather than a platform component**. The
latency figure is a measurement output, not a promised constant.

The canonical command model (`wamn-ftfc.1`), Git write adapter (`wamn-ftfc.2`), disposition
surface, per-root test isolation (`wamn-jole`), stored-suite fixture work (`wamn-m1om` and
`wamn-rktf`), and public projections (`wamn-ma5`) are frontend-independent. None waits for
a shell, Studio hosting, or `wamn-b454.1`.

This is an internal author-loop proof under the existing development administrator, not a
client-facing exposure: item 5 still gates retained client identity and client use. Item 3's
full execution-model verdict remains a separate gate after the v1 floors; the edit→run figure
above measures the loop and neither replaces that verdict nor promises its constants.

---

## 0 · Continuous obligations

Not sequenced, not "done" — each has an owner and a cadence, and each invalidates other
items when neglected.

### 0.1 Backlog reconciliation

~300 open records, most predating the callable-flow spec. **Time-boxed per item, not
programme-wide** — requiring all 300 reconciled before anything starts turns backlog
hygiene into a blocker:

> Before an item starts, records relevant to **that item** are reconciled — closed,
> re-anchored, or parked with a revisit trigger. Unreconciled legacy records are
> explicitly **non-authoritative**.

**Failure:** the relevant-record count grows while an item ships, which means the backlog
is a write-only log for that area. Roughly 70 records anchor to the previous plan's E1–E11
roadmap; whether that roadmap survives changes the shape of items 7–9.

### 0.2 Gate integrity

Gates backing shipped decisions variously cannot fail, soft-skip, or cover a fraction of
what they claim — and the suite is hand-run, so a decorative gate is undetectable.
**Success:** every gate backing a shipped decision has a mutation proof — a deliberate
break that makes it red — and the suite runs on a schedule rather than by hand.
**Failure:** a decision is found to rest on a gate that never could have failed.

This is foundational to a plan whose stated evidence model is "invariants live in code and
gates." Until it holds, every **Done when** below is unfalsifiable.

### 0.3 Fork maintenance

- **D23** — owned forks are first-class: immutable rev pin, a carried-commit ledger with
  per-commit exit conditions, and the upgrade-gate subset run and logged on every sync or
  added commit. No carried-commit ceiling. *Revisit if a base-version bump conflicts a
  carried commit on consecutive syncs.*

**The v2.6.1 upgrade is complete.** The fork retarget landed ahead of items
1 and 2A — both build *against* the runtime — as `wamn/2.6.1`, pinned at rev
`09b1132f`. It was a **policy re-port**, not a dependency bump: upstream
reworked the same files the fork patches. Surface absorbed: the crates.io
Wasmtime 47.0.1 family, `HttpServer`→`Ingress`, `AllowedIPNameLookups`, the
`wasmcloud:host` identity and cancel interfaces, and `host-component-plugins`
present but feature-disabled. The delta and base records remain in
`docs/archive/PLAN/WASMCLOUD-UPGRADE-2.6.1.md` and
`docs/archive/PLAN/WASMCLOUD-UPGRADE-2.6.0.md` pending their retirement decision. It
also put P3 on the table for item 1's streaming decision — taken there, and
declined in favour of the frozen P2 contract.

**New D23 cost:** v2.6.1 maintains **parallel P2 and P3 host surfaces**, so a policy at a
boundary implemented separately for both generations needs dual coverage — already
demonstrated by the UDP commit covering `host_udp.rs` *and* `host_udp_p3.rs`. Trace
injection is the HTTP instance of the same problem, and the strongest argument yet for
upstreaming that commit.

**Failure:** a sync that silently drops a carried commit, or a ledger entry whose exit
condition nobody can evaluate.

---

## 1 · Payload durability and the blob boundary

**Boundary.** Owns what is durable and where bytes live — including the **handle and
streaming contract** that item 2C's client blob node consumes. Does **not** own the run-state
transition surface (shipped with the POC), the client-facing blob node itself (2C), or its
authoring presentation (item 6).

**v1 goal.** Make the durable set the *resume point* rather than a log of everything that
happened, so **active recovery state is bounded by the nonterminal working set rather than
accumulated node history**, while replay seeds and observability remain separately
retention-bounded. The v1 floor uses bounded in-band payloads; moving bulk payloads out of
Postgres is the parked tail specified below.

**Position.** First, because item 3 measures the durable-run model and cannot produce a
meaningful number until capture-independent checkpoint recovery is settled: with capture on
the recovery path, the F1 scenario reports roughly the cost of storing the receipt document
once per node, which is a telemetry default rather than the execution model. Item 3 measures
the v1 floor and may trigger the parked payload tail; offload comparisons wait for that
reactivation rather than blocking the floor.

**Depends on** the POC. The v1 floor **blocks** item 3's recovery-model entry. The parked
payload tail — not checkpoint recovery — blocks the bulk/data-collection capability in
items 8–9.

### Why — the distinction that drives everything

**Three** things are stored per run, with three different scaling models, and conflating
them is the cost trap:

| | **Active checkpoint** | **Replay/audit seed** | **Capture** |
|---|---|---|---|
| Answers | where do I resume, with what data | what ran, with which inputs, versions, identities and effects | what happened, for a human |
| Read by | the executor | audit, replay, outcome retrieval | a person or the studio |
| Scaling | nonterminal working set | retained runs **and effect attempts** | nodes × runs × capture retention |
| Lifetime | removable at terminal | as long as the promised replay/audit surface requires | independent, usually shortest |
| Policy | none — always on | retention policy | immutable per-run admission: `full` or `off` |
| Fidelity | faithful and unscrubbed | authoritative | scrub-redacted, bounded, or absent |

Only the **first** is bounded by concurrency. The other two are retention-bounded, and
effect-attempt facts grow with execution history — so **item 3 measures the three classes
separately**, or a healthy checkpoint benchmark will conceal seed and capture growth behind
a claim that "durable storage is bounded by concurrency."

**Durability is the minimum authoritative state needed either to resume before an effectful
intent or to classify the run conservatively after one.** When a send may have completed but
its outcome was not recorded, the system deliberately declines to reproduce that outcome:
the run becomes `effect-uncertain` and no second dispatch is possible. The checkpoint is a
resume point — frontier, the payloads in flight *on* that frontier, occurrence counters, and
context. An effectful node's output is durable because it is the payload on the next token;
history is discarded because nothing downstream can observe it.

**What is live today:** `reconstruct()` rebuilds a run by replaying every completed node's
captured emission, and carries `ReconstructError::CaptureOff` to prove it — capture is
load-bearing for recovery. FLOW-SPEC requires the opposite (capture-exempt attempt state;
"capture `off` still recovers"). Boundary checkpointing is the replacement: its writers
exist, its reader is unwired, and `state_json` is read only for the parked-wake deadline.
This item finishes that migration.

### Why bulk cannot live in Postgres

The naive model is what DBOS does — one Postgres `TEXT` per step, capped by the 1 GB field
limit, no offload path. It fails here three ways: `state_json` rewritten whole at each
boundary multiplies WAL by payload × boundaries; accumulated history grows without bound;
and a 100 MB submission or a 1 GB API response has nowhere to go.

The convergent industry answer is claim-check. Temporal caps payloads at 2 MB with a
256 KiB default offload threshold and states plainly that payload writes are *not*
transactional — orphans occur and are cleaned by TTL. Step Functions caps at 256 KB and
tells authors to pass S3 ARNs. Azure's Durable Task offloads above ~900 KB transparently.

### The guest/host wall

Nodes execute inside WebAssembly components with bounded linear memory. **No node can
safely materialize an arbitrarily large payload** — bulk handling must use bounded
streaming. So the mechanism is a function of size, not a preference:

| Payload | Mechanism | Where the bytes live |
|---|---|---|
| below threshold | inline value | guest memory → checkpoint |
| threshold … ceiling | **platform offload** — host-side, on the emission path | guest memory briefly → blob |
| bulk, client-directed | **streaming blob node** — guest orchestrates, host transfers | bounded chunks transit the guest; the complete object is never materialized there |
| above ceiling | typed, catchable rejection | — |

**Decided (wamn-4u7p.3): activate the existing P2 streaming contract in
`wamn:node@0.1.x`; do not introduce a P3-native 0.2 in item 1.** The durable dataflow value
is `payload-ref`, a plain run-scoped storage handle that crosses node and checkpoint
boundaries without moving bytes. A node opens that handle only when it actually consumes or
produces bulk data, through the optional `payloads` import. Its P2
`wasi:io/streams` resources provide bounded, backpressured transfer without waiting for
`component-model-async`, so they meet the guest-memory boundary the blob design requires.

The exact pinned wasmCloud v2.6.1 fork (`wash-runtime` rev `09b1132f`) does put P3 on the
table, but it does not supply a zero-copy storage-handle transfer. Its cross-store
`stream<T>` relocation builds a live, no-buffering channel pump, keeps the producing store
alive, and applies a drain timeout. That is useful for P3 component-to-component signatures;
it does not improve on passing WAMN's opaque `payload-ref` as a value and opening the backing
object only at the endpoint. Moving to 0.2 now would instead add a breaking client/SDK/world
migration and another P2/P3 host-policy surface before a measured requirement needs it.

Activation first aligns the currently inert `wasi:io` version pin across the authoritative
WIT, generated copies, and the pinned host, then freezes that strict world in resolved-node
identity. The host resolves handles under the active run/project/environment, streams
directly to or from the platform store with ceiling enforcement, and exposes no store
location or credential. Runner and SDK adapters preserve a streamed reference without
materializing the complete object. `wamn:node@0.2` remains the coordinated WASI 0.3/native
async revision (`wamn-72i`), revisited only when the target ABI and service lifecycle are
stable and a measured cross-component streaming case justifies migration.

**Offload does not solve guest memory, and the split matters.** Host-side offload fixes
checkpoint size, WAL amplification, and database bloat — but a node that constructs a 100 MB
value and returns it has already materialized it in guest memory before offload sees it.
**Transparent offload** serves medium values that fit safely in the guest but should not sit
inline in durable state; **streaming blob nodes** serve genuinely large values whose bytes
never become one materialized guest payload. Only the second is a bulk mechanism.

### Platform offload versus the client blob node

These share a storage technology and nothing else. Keeping them distinct is the point.

| | Platform offload | Client blob node |
|---|---|---|
| In the graph | invisible | an explicit node |
| Capability | none — host-side | declared, structural, egress-gated |
| The object is | an implementation detail of a payload | a business object |
| Lifetime | run + retention, then collected | the customer's, until they delete it |
| Reachable from | the owning run only | their other flows, API, frontend |
| Billed as | execution overhead | stored data |

**Separate namespaces, credentials, and buckets.** Platform GC must be structurally
incapable of deleting a client object; a client structurally incapable of enumerating or
pinning platform payloads. Prefix separation under shared credentials is one bug from
both.

**A content digest is an object identity, not an authorization grant.** Global
content-addressed deduplication must not quietly become cross-project access, retention
coupling, billing coupling, or an equality side channel — observing that an upload deduped
tells you someone else holds those bytes. **Authorization, GC reachability, encryption, and
accounting are scoped at least per project environment**, even if physical deduplication is
later implemented below that boundary.

They are complements: an author handling a 100 MB document uses the blob node
deliberately, so the payload was never large — leaving platform offload to catch only the
accidental and spiky cases. This is the same platform/client split already drawn for
Postgres (`wamn_run` vs project schema), HTTP egress, credentials, and events. **Every
shared primitive gets this split**; blob is its newest instance, and the next one
(brokers, time-series) inherits the checklist.

### Rules this establishes

- **Capture is never load-bearing.** Recovery reads the authoritative boundary checkpoint and,
  for external effects, the immutable effect ledger. `node_runs` is a reconstruction projection;
  it carries no attempt authority, and recovery never depends on a payload column.
- **The completion write and the boundary checkpoint commit together.** No observable state
  where an attempt is `success` and the checkpoint predates it — the output would exist
  only in a column that may be absent.
- **Blob first, then the reference.** Ordering, not atomicity, is the safety property: an
  orphaned blob is recoverable garbage, a dangling reference is an unrecoverable run.
  Content addressing makes retries idempotent and orphans safely collectible — an orphan
  still occupies storage until GC reclaims it.
- **Two GC mechanisms, not one.** *Referenced-object retention* is governed by authoritative
  reachability: an object survives while an active checkpoint, a retained replay/audit seed,
  or a retained caller outcome refers to it. A duration-based rule would collect payloads a
  promised replay still needs. *Uncommitted-orphan collection* is governed by TTL or a sweep
  and applies only to objects written but never referenced — the blob-before-checkpoint
  window. The algorithm is this item's decision; the two must not be conflated.
- **Three retention classes, not two.** Terminal completion does not license deleting
  everything:

  | Class | Contents | Lifetime |
  |---|---|---|
  | **Active recovery checkpoint** | frontier, live payload references | removable at terminal |
  | **Retained replay seed** | admitted input or its reference, the pinned artifact and execution bundle, invocation context, effect-attempt facts | as long as the promised replay, audit, outcome-retrieval, and run-investigation surfaces require |
  | **Observability capture** | human-facing evidence | optional, independent, may be scrubbed, sampled, or absent |

  Capture cannot be the fallback for the middle class — it is declared optional and
  non-authoritative, so anything replay depends on belongs to the seed. **Consequently
  platform blob GC follows *replay* reachability, not live-checkpoint reachability**; the
  earlier "unreferenced by any live checkpoint" rule would collect objects a promised replay
  still needs.

  > **Retention guarantees reconstructability; it does not by itself grant permission to
  > re-execute historical code or effects.**

  > **One retention rule, three instances (and counting): anything a retained replay seed
  > references is immutable and retained while referenced** — execution bundles, payload
  > blobs, and connection-instance definitions today. Secret material is the deliberate
  > exception: credentials keep an independent revocation lifecycle, and an unreacquirable
  > generation is an explicit refusal rather than a substitution.

  That one rule is what keeps blob GC, bundle retention, emergency revocation, platform
  upgrades, and the author-facing meaning of "Replay" from acquiring incompatible
  interpretations — a retained bundle is not executable merely because its bytes exist.

  **Decided (wamn-4u7p.1): three author operations, with “Replay” reserved for controlled
  execution.**

  | Operation | Executes? | Definition and authority | Durable result |
  |---|---|---|---|
  | **Audit reconstruction** (“Inspect original”) | No | Projects the authoritative retained seed and attempt facts, even when an executable or credential is now unavailable or revoked | A read-only audit projection with explicit unavailable/revoked markers; no run |
  | **Replay** | Yes, controlled only | Uses the exact pinned artifact, bundle, occurrence input, and seed, subject to current platform executable admissibility | An isolated scenario report, never a production run |
  | **Run again** / **live re-execution** (event-plane **Reprocess**) | Yes, in production | Performs fresh admission from the entry under the current active release or registration, principal authorization, revocation, connections, credentials, idempotency, and effect policy | A fresh production run with typed lineage to its origin |

  Retained bytes are evidence, not permission. Controlled Replay refuses a prohibited or
  unavailable pinned executable or credential generation and never substitutes a current
  definition. It runs with an ephemeral database, deterministic clock and randomness,
  fixture-only credentials, and doubles or recorders that cannot reach live egress or emit
  production business events. Arbitrary mid-graph seeding and partial rerun exist only in
  that scenario boundary.

  Live re-execution always starts at the entry transition and never claims historical
  equivalence. Durable lineage records the operation kind, origin, and selected definition:
  controlled Replay produces scenario provenance, live re-execution produces a production
  run, and audit reconstruction produces no run. Item 8 applies the same split to events:
  historical pinned processing is controlled Replay; processing retained input with the
  current definition is explicitly Reprocess/live re-execution.
- **The checkpoint is unscrubbed by necessity** — scrubbing a frontier payload resumes to
  different results. Scrubbing is a capture convenience and **not a security boundary**;
  secrets travel by credential reference and must not ride in payloads.

### v1 effect posture — one attempt, one dispatch

Settled 2026-08-04 and narrowed by `wamn-0h0g.4.9`. The platform ships one
conservative effect protocol; no strengthened effect-retry tail exists.

**In force:** a pure occurrence writes no effect-ledger row. An effectful
occurrence has one immutable write-ahead attempt, at most one immutable
dispatch fact, and a terminal outcome when one is known. The attempt records
the occurrence identity plus the exact plan, connection, and credential facts
authorized before the send. No engine-generated outbound retry token or
endpoint-behavior assertion participates in the protocol.

The run-state API is the sole ledger write path. Retrying an attempt or outcome
builder is exact-idempotent: the same complete facts return the existing row;
different facts refuse. The dispatch relation's occurrence key makes the first
successful insert the sole wire-I/O permit. The database cannot observe wire
I/O, so attempt-before-send remains a run-state chokepoint invariant and the
integrated crash proof owns it.

**Delivery split:** `wamn-0h0g.4.9` lands the inaccessible ledger primitive,
private run-state API, and database proofs without a production caller.
`wamn-0h0g.5.4` activates the private adapter and owns the integrated
attempt-before-send, one-dispatch, outcome, and pure-no-row proof.

**Automatic behavior is single-shot.** A known external failure fails the run.
A sent attempt without a recorded outcome is `effect-uncertain`; it never
sends again. More conservatively, reclaim seeing any abandoned effectful
write-ahead intent removes queue eligibility and marks the root run
`effect-uncertain` without invoking the flowrunner. A later genuinely new
admission may repeat the external effect, but it is a new caller decision and
a new run.

**The v1 operator surface terminalizes; it never supplies an effect outcome.**
`get-run` exposes the uncertain state and immutable ledger facts. The one
repair transaction locks the affected facts, verifies `effect-uncertain`,
appends an immutable `operator_run_actions` row with basis, evidence reference,
correlation, principal, and prior state, and marks the node and run terminally
failed. There is no success assertion, continuation, bulk selection, successor
attempt, or silent re-execution.

### Exit gate — one effect policy across standard and custom nodes

The canonical `ResolvedNodeContract` carries only `pure | effectful` for this
purpose. Standard descriptors and custom manifests resolve through the same
field; publication includes it in artifact and bundle identity. The runtime
may not infer safety from an HTTP method, mutable node configuration, or
environment assertion. A descriptor change mints a new artifact/platform
identity; an already-published artifact is never silently reclassified.

**Checkpoint recovery must not foreclose fan-out.** *Not* an exit gate — the engine cannot
fan out: `Emission` carries one port and P1 allows one edge per port, so a completion yields
exactly one successor token. The hazard (a nondeterministic producer feeding two branches,
one checkpoints, crash, the other replays the producer and sees different bytes) needs two
branches to exist. Without them, re-execution before a boundary just proceeds with fresher
data. Design the recovery model so fan-out remains addable; do not block item 1 on it.

### Done when — v1 floor

A run recovers correctly with capture `off`; pure occurrences create no effect
facts; each effectful occurrence creates one immutable attempt and at most one
dispatch; a sent attempt without a recorded outcome becomes `effect-uncertain`
and is never resent; audit reconstruction does not execute; and operator
terminalization records one typed, immutable action without asserting success.

**Parked tail exit, on reactivation:** over-threshold payloads ride as references with the
bytes in the blob store; a crash between blob write and checkpoint leaves an orphan and a
resumable run, never a dangling reference; GC reclaims orphans without reaching client
objects; and the same F1 scenario measured before and after shows the WAL and storage
difference the model predicts.

### Failed if

Recovery cannot be made capture-independent without also re-architecting the engine — that
would mean the checkpoint is not a sufficient resume point and the *model* needs revision,
not the code. **Parked-tail failure case:** if offload latency proves unacceptable even above
the measured threshold, the tail moves toward a much higher threshold and a hard payload
ceiling instead.

### Decision points

- ~~**Inline threshold and hard ceiling**~~ — **Deferred (2026-08-04):** the
  v1 floor is bounded in-band payloads on the frozen `wamn:node@0.1.x`
  contract; the two numbers are set by measurement when the parked tail
  below reactivates.
- ~~**Does capture keep serving the studio's run view, or move to the
  telemetry pipeline?**~~ **Settled (2026-08-04) as status quo for v1:**
  capture keeps serving the author's run history as platform data — a
  product surface, not an SRE one. Relocation is revisited only with the
  parked tail, and any relocation must preserve durable seeds for parked
  controlled Replay.
- *(Resolved: the client blob node belongs to **2C** — it is a node-authoring and catalogue
  deliverable, with item 6 owning only its authoring presentation. **Item 1's obligation is
  to leave behind the handle and streaming contract 2C's blob node consumes.** Its arrival
  trips `egressbench`'s `wasi:blobstore` justification by design.)*
- ~~**GC by TTL or by reference sweep**~~ — **Deferred (2026-08-04):** the parked tail
  resumes from the two-mechanism envelope above: replay-reachability collection for
  referenced objects and age-based collection for uncommitted orphans.
- **Does a non-deterministic node followed by fan-out force persistence?** Re-executing a
  fetch after a crash is normally safe, since nothing past the boundary committed — but
  under fan-out, one branch may already have checkpointed with the old bytes, and
  re-execution hands the other branch different data. A validation-time check is possible.

### Alternatives

| Option | Buys | Costs |
|---|---|---|
| **Checkpoint-only recovery + claim-check offload** (recommended) | Storage and correctness policies become independent; bounded working set; bulk possible at all | Occurrence counters must be stored explicitly; purity lies now repeat effects; two stores in the durable set |
| Keep per-node replay; make capture mandatory | One mechanism; full history free, which is a product feature for authors | Storage scales `payload × nodes × runs`; the flows that most need capture off are exactly those that cannot turn it off |
| Everything inline in Postgres (the DBOS model) | Single store, single transaction, simplest recovery | Cannot accept a large document or an API that returns one; WAL and vacuum debt scale with payload |
| Externalize the whole checkpoint | Uniform | Object storage on every boundary — unacceptable latency, and an availability dependency on the recovery path |

### Configuration

| Setting | Scope | Default | Notes |
|---|---|---|---|
| `payload.inline-threshold` | env | 256 KB *(to confirm)* | above it, offload |
| `payload.ceiling` | env | *(to set)* | above it, typed rejection rather than degradation |
| `payload.compress` | env | on | JSON typically 5–10×; moves the crossover materially |
| `payload.store` | env | platform blob namespace | distinct from client storage |
| `runs.capture_mode` | immutable run admission | direct draft-run: `full`; every published or test-set run: `off` | `full` \| `off`; never derived from mutable flow or environment state |
| capture output ceiling | platform writer | 64 KiB | write-side only; over-ceiling `full` output is NULL while size and optional hash remain, so reads derive `output-too-large` without consulting the current ceiling |
| `blob.retention` | env | **reachability-governed** — retained while referenced by an active checkpoint, a retained replay/audit seed, or a retained caller outcome | platform GC only; *not* a duration |
| `blob.orphan-ttl` | env | *(to set)* | a separate mechanism: collects objects written but never referenced — the blob-before-checkpoint failure window |

### Testing

**Correctness**
- capture `off` → a crashed run recovers identically to capture `full`
- kill after the send and before the outcome write → the sink sees one effect,
  reclaim yields `effect-uncertain`, and no second dispatch is possible
- kill between the blob write and the checkpoint commit → orphan exists, run resumes from
  the previous boundary, GC reclaims it
- payload above the ceiling → typed rejection, no partial write, no run
- content addressing: one payload across five boundaries stores one object; a re-executed
  node rewrites nothing
- isolation, both directions: platform GC cannot reach a client blob object; a client flow
  cannot enumerate platform payloads
- a `full` run stores scrub-redacted author history while the independent checkpoint retains
  the faithful resume payload — asserted, so capture is documented as a presentation floor,
  not a secret-classification or recovery boundary

**Threshold characterization** — sweep payload size 1 KB → 10 MB logarithmically, at
several concurrencies, against an in-cluster store and an external one, measuring boundary
latency, **WAL bytes**, table and TOAST growth, and vacuum pressure.

The measurement must be **sustained, not single-request**. Latency favours inline well past
the point where WAL amplification and bloat say otherwise, because latency is paid per run
while vacuum debt accumulates across them; a burst-only test sets the threshold too high
and the cost surfaces weeks later as autovacuum falling behind. The existing
`queuebench --mode ceiling` work is the right shape. Compression is measured in the same
sweep, since it shifts the crossover.

---

## 2 · Componentization — node authoring, composition, and connections

**Boundary.** Owns what a flow is composed of, what it may therefore do, and where
external endpoints are configured. Does **not** own the flow-authoring UX (item 6) or
release membership mechanics (item 7).

**Goal.** Cash the reason wasmCloud was chosen: a flow's capability set is decided by
**what it is composed of**, not by what its runner happens to import — and new capability
arrives as authored components rather than vendor roadmap.

**Position — moved ahead of measurement and authoring.** It **changes the execution
model**: what runs, what a flow's capabilities are, how long a first-run-of-a-node-set
takes, and whether the tested artifact equals the deployed one. Measuring the interpreter
(item 3) or building a loop against it (item 6) and then switching to composition is
rework where H1 makes rework expensive. It develops against `ctl` and fixtures, the way
the POC does — a studio is convenience, not a dependency. It also blocks item 10's starter
applications: a receiving vertical cannot ship if the connector it needs does not exist.

**Only 2A blocks**, and each sub-item carries its own gate — treating four programmes as one
prerequisite would make the whole component ecosystem gate product learning:

| | Scope | Blocks | Done when |
|---|---|---|---|
| **2A** bundle specialization: packaging + economics | granularity, capability worlds, runner specialization | **3, 6A** | the packaging shape is chosen from measured evidence and a capability-bearing node composes |
| **2B** connections | the env/artifact boundary, one real protocol | 6A's outbound journey | the identical artifact runs against distinct env connections |
| **2C** node authoring + distribution | harness, installation, palette, distribution | the connector catalogue, item 10's templates | an authored node is built, installed, and appears in the usable catalogue |
| **2D** custom-node composition + patchability | compose-in vs D7's hop; fleet maintenance | nothing near-term | the posture is chosen from fleet evidence |

**Depends on:** 2A on nothing beyond the entry gate. **2C splits** — the core authoring and
distribution product (local harness, signed build, project installation, catalogue feed, a
small non-bulk connector) is independent of item 1; only the **bulk-capable node contract
and the first bulk connector** need payload handles. The catalogue-growth path does not wait
on blob durability. 2B is independent of 2A in principle —
the connection boundary matters under either execution shape — though landing 2B first means
touching node config resolution twice.

### 2A · Composition — capability by construction

**Decided.** Nodes have strict component contracts; execution-bundle identity is explicit;
draft and published runs use the same execution path.

**Direction (gated by 2A).** **Execution-bundle specialization** becomes the default,
making the outer capability ceiling structural rather than code-enforced. The plan stays *data*, so
composition is a WAC **link** — no source compilation and no language toolchain, though a
**pinned composition tool** still produces the bundle and therefore participates in
reproducibility and supply-chain identity — and interpretation survives while the capability
surface narrows. §14 of the flow spec records it: *"plan as data, schema as
validator, **node set as imports**."*

**Candidate.** One executable plug per capability class.

**The load-bearing ambiguity 2A must resolve.** The spec's phrase above — "node set as
imports" — and "one plug per capability class" are not automatically compatible, and the
difference is architectural:

| | Guarantee | Consequence |
|---|---|---|
| **Capability-class specialization** | exact capability *classes* are structurally narrowed | a plug may carry unused node implementations; adding a node already inside a present plug may require **no** recomposition; adding an implementation *to* a plug changes its digest for every bundle using it, including flows that never call it |
| **Exact node specialization** | only the selected implementations enter the bundle | requires per-node or selectively linkable packaging; more artifacts to build, sign and distribute, and longer links |

2A must be built to **distinguish** these, not merely to measure one. (The combinatorial
objection to per-node plugs is weaker than it first appears: only bundles that actually
*occur* get composed, so distinct bundles are bounded by observed usage rather than by
2^N. The real per-node costs are artifact count and link time; the real per-class cost is
shared fate inside a plug.)

**This rests on evidence, not intent.** `wamn:node@0.1.0` is a **frozen ABI** whose own
status note records that the handler cleared the S4 spike *cross-language and wac-composed*;
`nodebench` carries the composed arm as a gate of record; `flow-driver` produces a live
`flow-composed.wasm` via `wac plug`; and `export_node!` componentizes any `Node`
implementation, which is the same trait the standard library is written against. D1's "later
opt-in backend" framing is what this item retires — **opt-in composition would make
least-privilege opt-in**, which inverts the posture.

**Three things the phrase "node component" conflates**, and they are not the same
architecture:

| Term | Meaning |
|---|---|
| **Node type** | the author-visible semantic operation and its strict interface |
| **Executable plug** | the component *packaging* unit — one node type or several related ones |
| **Execution bundle** | the specialized runner plus the exact plugs and adapters selected for a flow |

"One node = one component" and "one capability class = one component" are both statements
about **plugs**, and they differ.

**What 2A actually decides — design and economics, not feasibility:**

- **Packaging granularity — an isolation decision, not a cache optimization.** One plug per
  node gives N plugs and combinatorial bundles; **one per capability class** — pure, http,
  postgres, later messaging — gives a handful, so bundles are subsets of a small set,
  composition count collapses, and cache hit rate rises. But the plug boundary is also the
  **fault and trust isolation unit, the patch blast radius, the revocation unit, and the
  independent-upgrade unit**: two node types sharing a plug share a linear memory and a
  release cadence, so a compromise or a bug in one reaches the other. Coarse packaging trades
  isolation for cardinality, and that trade — not the cache — is what 2A must weigh.
  *Candidate: per capability class.*
- **Cache cardinality under platform upgrade.** Bundle identity includes runner/platform
  revision (see the draft-pinning rule in item 6A), so **every platform upgrade invalidates
  every bundle** and forces recomposition. With capability-class plugs that is a handful of
  recompositions; per-node it could be hundreds. An operational cost, and a further argument
  for coarse packaging.
- **Capability worlds.** A no-caps world exists and a pure node composes on it. A
  capability-bearing world must become a composition target too — one per class, or one
  carrying all. **The sharpest open question.**
- **Economics.** Composition latency, cache hit rate, cold start, memory, and per-invocation
  overhead against the linked path. Unmeasured; the real gate.
- **The platform-upgrade recomposition event — measured, not argued.** Since platform/runner
  revision participates in bundle identity, an upgrade invalidates every affected bundle.
  That event matters more than single-bundle composition time, and it is where packaging
  granularity may differ materially:

  ```
  compose the fleet's observed bundle set under a new platform revision
  retain old bundles while runs still reference them
  make new bundles available before routing new work to them
  avoid a simultaneous cold-start or unavailable-bundle window
  measure cache rebuild time and registry pressure
  ```

  This belongs in the exact-node vs capability-class comparison, not only in its prose.

*Terminology, used consistently below:* the named unit in identity, caching, rollout,
retention, and measurement is the **execution bundle**, and the target is
**execution-bundle specialization**. "Per-flow composition" survives only as informal
shorthand — it wrongly suggests one artifact per flow, recomposition on every edit, flow
lifecycle owning cache lifecycle, and no reuse across projects, all of which this plan
rejects.

**What survives if 2A's economics fail.** The fallback is not "reopen componentization" — it
is a linked runner with everything else intact:

```
survives:  nodes remain strict, signed executable contracts
           execution-bundle identity remains explicit
           draft and published runs use the same execution path
           connections remain environment-scoped
           per-node capability narrowing remains enforced (intra-runner)
           flow artifacts remain independent of execution placement

lost:      the flow-level capability ceiling encoded in a composed
           component's imports — narrowing stays code-enforced, not structural
```

Exactly one property is lost. Saying so is what lets 2A genuinely fail without becoming a
programme crisis that reopens node authoring, distribution, artifact identity, connections,
and the authoring architecture.

**Compositions are keyed by execution-bundle identity, not by graph** — so editing
expressions, rewiring edges, and changing configs recompose nothing. An author waits for a
composition when they use a bundle new to the environment, and never otherwise.

**Identity is the bundle, never the node-type names.** Two flows naming `http-request` at
different component digests must not share executable bytes. Identity includes node types
**plus component digests, runner/platform revision, interface/WIT versions, and any
adapters** — anything that changes the composed bytes changes the key.

**Cache scope is not universal, and neither is its measurement.** A globally
content-addressed cache is right while only first-party components participate. Once private
components do, **reuse must respect registry authorization and org visibility even when the
digest matches** — a digest hit is not an entitlement.

So the economics are measured in two places, never aggregated:

```
2A   first-party bundle economics, global eligible reuse
2D   private/custom bundle economics, entitlement-scoped reuse
     (only if custom nodes compose in)
```

A favourable 2A hit rate over globally shared first-party plugs must **not** later be cited
as evidence for custom-node composition: private components can produce far higher bundle
cardinality and far lower *authorized* reuse even when byte digests repeat. This does not
move custom-node economics into 2A; it stops one aggregate metric from deciding 2D before
fleet evidence exists.

A flow that calls SAP, joins project data, and returns CSV composes to a component
importing `wasi:http` and `wamn:postgres` **and nothing else** — no filesystem, sockets,
messaging, or blobstore. Not refused at runtime: *unrepresentable*, which is the house
discipline applied to the runtime itself.

**What this dissolves**
- The standard-vs-component argument. The capability union was its strongest column;
  per-flow imports delete it, so **adding a new capability class widens nothing for a flow
  that does not select it** — MQTT, OPC-UA, SFTP. Growth *inside* an existing plug is
  different: it still carries code, patch, and release blast radius for every bundle using
  that plug.
- `egressbench`'s union concern, for flows: the first-party runner's import list stops
  being a proxy for what tenant code can reach.
- `allowed-hosts` as the primary egress control; it becomes a narrowing policy inside
  `wasi:http` rather than the boundary.
- Possibly D7's HTTP hop: a custom node composed **in** rather than hopped **to** removes a
  signed round trip per invocation, and component instances keep separate linear memories,
  so intra-flow isolation survives composition. (2D.)

**Three different concurrencies, routinely conflated:**

```
many runs, one process     pool of instances      ✓ wanted — this is density
many tasks, one instance   P3 task concurrency    ✗ shared linear memory + context
one run, many nodes        parallel dispatch      — separate engine question
```

**Instance pooling is the density mechanism — and it is wamn's to build.** An executor
holds one `ExecutionHost` and drives claimed runs through `&mut`, so **one replica executes
one run at a time** and request concurrency scales only by pod count. A pool of instances,
each with its own store and `ExecutionState`, is what lets one process run many at once —
relevant to item 3's throughput measurement, not only to bundle economics. Note that
upstream's `pool_size` covers linked calls and P3 HTTP dispatch, **not** a manually
constructed `ExecutionHost`: 2A must establish which upstream path is even on wamn's
execution path rather than assume a config flag parallelises the executor.

**Task concurrency inside one store is declined** — and not for ordering reasons.
`ExecutionState` holds `current: Option<Active>` and the loop pops a token only when it is
`None`, so one node is active per run by construction. Two runs sharing one store would
share its linear memory and its mutable credential and egress context. Pooling gives
density without that; task concurrency gives the hazard without the density.

Ingress is the opposite case: `flow-http` serves many independent callers, so concurrency
there has a real consumer — and handling requests serially is the same defect as H9's
sequential accept loop.

**What it does *not* dissolve.** Per-node capability narrowing already exists inside the
runner — dispatch narrows the capability facade to each node's declared row, so a buggy node
cannot reach an undeclared capability. Composition moves that boundary **outward** to the
guest edge: structurally enforced rather than code-enforced, inspectable from the artifact,
and proof against runner bugs rather than only node bugs. Real, and worth stating honestly
when weighing 2A's economics.

**Where composition runs:** a composition service (the builder extended, or its sibling) —
it needs registry credentials to push, and the cache must be shared across authors and
projects to reach the hit rate that makes the loop viable. Idempotent and
content-addressed, so concurrent requests for one execution bundle collapse.

### 2B · Connections — the env boundary for anything external

**The structural problem.** Flow artifacts are keyed `(tenant, flow_id, flow_version)` with
`tenant = org:project` — **no env** — so the same artifact is referenced by the dev and
prod releases and its bytes are identical across environments by construction. Releases,
by contrast, are env-specific (promotion mints a fresh version in the target env). The rule
that follows: **anything env-specific belongs in a release member, the vault, or config —
never in the artifact.**

Credentials already obey this. **Outbound endpoints do not:** `http-request` takes `url` in
node config, expanded from payload and context but never from env configuration. So
pointing prod at the prod endpoint requires different bytes → a different `graph_hash` → a
different flow version, and **dev tests `v3` while prod runs `v4`** — an H1-class violation
arriving exactly at the promotion boundary.

**The fix: connections**, naming what always travels together — endpoint, credential,
defaults. The flow names *which* system; the environment says *where it is and how to
authenticate*. Promotion carries the identical artifact.

**Three concepts, one ownership boundary.** The plan previously said both that connections
are env-scoped release members *containing* the endpoint and that a release *declares
requirements the environment satisfies* — which are compatible only once these are
separated:

| | What it is | Owned by |
|---|---|---|
| **Connection requirement** | a portable declaration — *this application needs an HTTP ERP connection* | the artifact / template |
| **Connection instance** | endpoint, TLS posture, proxy policy, credential-set handle | the **environment** |
| **Connection binding** | associates a logical requirement with an instance | the env-specific release |

> **Portable artifacts and templates declare typed connection requirements. Environments own
> connection instances. An environment-specific release binds requirements to instances.**

Without that split, two sources of truth emerge: credential rotation or endpoint failover
could accidentally become release changes, template portability cannot tell a declaration
from supplied material, and promotion validation cannot say precisely what is missing. The
table layout is 2B's design; the ownership boundary is settled here.

**Layering (deliberately kept coarse for now):** the connection is the base and supplies
defaults; a flow appends or overrides. Some fields will not be flow-fillable — the
authority half of a URL, the credential, TLS posture — because those decide *who you are
talking to and as whom*, which is the env's business, while path, method, query and body
are the flow's work. **The precise per-field boundary is deferred**; the coarse rule
(env defaults, flow appends or overrides) is what this item builds against.

**Egress derives from connections** rather than being author-declared: a flow cannot reach
a host it has no connection for. But connections are **application-level authority**, not
the outer ceiling — they say *this flow may use the ERP connection*, and they sit inside
platform host policy and cluster network policy rather than replacing them:

```
effective destination  =  connection-defined authority
                          ∩ platform host policy
                          ∩ cluster network policy
```

**Authority is evaluated through one canonical model and cannot be widened by redirect, DNS
resolution, or proxy behaviour.** The resolver is 2B's design; the property is what its tests
must establish.

**HTTP `0.1` floor decisions (`wamn-ko5r.8`).** The artifact spelling is a leading-slash
connection-relative target such as `/holds`; bare `holds`, raw `//`, absolute authorities,
and base-path escapes are invalid. Exactly one normalizer strips that one leading slash and
constructs the canonical target accepted by the resolver and the private adapter.
The v1 transport is direct-only. A generation declaring proxy transport is typed
`incompatible` both during staged-generation validation and again at dispatch; it never
falls back to direct. CONNECT/TLS proxy transport is demand-gated separately by
`wamn-ko5r.32` when a named environment requires proxied egress.

An earlier draft said connections *supersede* an env-level allowlist. They do not; they are
the innermost of three layers.
Symmetric with what exists, stated precisely now that the three-way split exists:
**auth-source definitions govern inbound admission; connection *bindings* govern outbound
access.** Operational credential material and connection **instances** stay environment-owned
and are *not* release members — saying "connections are release members" generically would
invite mutable endpoints and credential generations into releases.

**The simple/reporting authoring surface (item 6) is gated here.** A report flow needs
formatter nodes (CSV, XLSX, PDF) and delivery nodes (email, SFTP) that do not exist —
each a node *and* a connection type. That makes the operations persona's entry point a
capability question owned by this item, not a UI question owned by item 6.

**Durable effects record the connection-instance generation they used.** Connection
instances are deliberately operationally mutable — endpoint failover and credential rotation
must not mint a flow artifact or a release. Every external effect attempt therefore records
the connection requirement, immutable instance generation or definition hash, and credential
generation authorized for that occurrence — never secret material.

The floor adapter uses write-ahead ordering: one immutable attempt records the trusted
`(tenant, run, frame, node, occurrence)` coordinates, current plan, source artifact,
requirement, and selected connection and credential facts before any send. The first dispatch
insert for that occurrence is the sole wire-I/O permit; a terminal outcome is appended when
known. A sent attempt without an outcome is `effect-uncertain` and is never sent again. Caller
claims carry identity only; release, binding, generation, and authorization facts are derived
from the admitted run and catalog state.

**Plane boundary (settled 2026-08-05).** Node placement and execution
transport are platform-plane: a flow references pinned implementation
identities, never endpoints — node configuration cannot carry an endpoint
or any absolute URL — and connection-backed HTTP and flow-level
`allowed-hosts` are mutually exclusive. Business egress is a portable
connection resolved through an environment binding; invoking a custom node
is internal execution transport through the trusted host runtime, with
placement and signing host-owned.

**Decision (wamn-ko5r.1, narrowed by `wamn-0h0g.4.9`): generation
pinning is per attempt, and an attempt never redispatches.** Exact-generation
availability and authority are verified before the first send. A later distinct
occurrence resolves the currently active compatible generation and records it
independently. An uncertain attempt needs its recorded non-secret generation
facts for audit, not retained credential material for another send.

**It composes with the activation gate**: occurrence 2's generation is guaranteed compatible
because an incompatible one never activates, and a failed activation leaves the previous
generation in place rather than disabling the binding. Occurrence 2 finds **no** active
generation only after a deliberate operator disable — and that is an explicit failure, never
a fallback to a weaker one.

**A recorded generation remains audit-resolvable.** Its non-secret definition is immutable
and retained while an active attempt or retained audit seed refers to it. Credential material
has an independent revocation lifecycle; ledger retention never extends credential validity.

**Decision (wamn-ko5r.3): activation is a serialized all-bindings compatibility commit.**
Failover and rotation are operational rather than release changes — but a generation can
also alter TLS or proxy requirements, authentication behaviour, authority scope, and redirect
policy. A proposed generation is therefore immutable and staged; it is never made current by
an unchecked configuration write.

Activation serializes per instance and takes one validation snapshot containing the
expected active-generation pointer, every active binding and its portable requirement, and
the referenced connection-contract, credential-kind, platform-host-policy, and
cluster-network-policy revisions. The candidate must pass, in order:

1. **Intrinsic definition validation:** exact supported connection type and contract;
   every contract-required field present and every unknown or environment-forbidden field
   absent; every primary, failover, and proxy authority canonical and unambiguous; TLS
   verification, TLS name/identity, redirect scope, and proxy transport consistent with
   those authorities; and an existing credential-set reference of a contract-permitted
   kind. Secret material is neither copied nor inspected.
2. **Outer-ceiling validation:** every declared destination, failover target, literal
   address, and configured proxy is admissible under both snapshotted outer policies. This
   proves that the definition cannot widen policy; it does not replace dispatch-time DNS,
   redirect, proxy-target, or current-policy enforcement.
3. **Every-active-binding validation:** type and exact contract match; required fields and
   portable authority constraints are satisfied; and the credential kind matches.

An instance with no active bindings may activate after the intrinsic and outer-ceiling
checks pass; a later binding still has to pass ordinary publication or promotion validation.

The active pointer changes by compare-and-swap only if **all** checks pass and every
snapshotted input is still current. A changed pointer, binding set, requirement, policy,
or credential-kind record makes the proposal stale and refuses activation; the
operator may retry against a fresh snapshot. The commit records the candidate definition
hash and the identities of the validated inputs so the decision is auditable.

> **Any intrinsic, policy, binding, or stale-snapshot failure preserves the
> status quo:** the candidate does not replace the current generation, the existing
> compatible generation stays active, and no binding is disabled or forked automatically.
> Disabling an affected binding or creating a differently scoped instance requires an
> explicit operator action, followed by a new activation attempt where applicable.

Automatically disabling working bindings because someone proposed a bad endpoint, TLS
configuration, proxy, or credential would turn validation into an outage mechanism.
Activation is also not a perpetual certificate: dispatch still enforces current authority
and outer policy.

This sharpens promotion's role: promotion validates that the target binding is compatible
*initially*; generation activation preserves that compatibility after environment changes.

**Decision (wamn-ko5r.4, superseded by `wamn-0h0g.4.9`): endpoint
behavior never strengthens effect dispatch.** HTTP `0.1` has no outbound
deduplication claim, evidence policy, or engine-generated retry token. The
platform validates only durable facts it can own: connection contract,
canonical authority, TLS/proxy posture, credential kind, outer policy, and
the active immutable generation. Remote endpoint behavior may inform an
operator, but it is neither artifact identity nor dispatch authority.

**v1 scope.** The trusted adapter, write-ahead attempt, one-dispatch fact,
typed refusals, staged CAS activation, and exact-generation refusal are the
complete floor. There is no parked strengthening path.

**It also sharpens H1.** The tested-bundle invariant proves **executable identity**, not
environmental behaviour — dev and prod deliberately bind different connection instances, so
identical bytes do not establish that the production ERP behaves like the sandbox. Target
connection compatibility is separate evidence (smoke validation at promotion), and H1 should
not be read as covering it.

**Connections are typed, and the type set grows with the node catalogue** — `http-request`
needs an `http` connection, a future `mqtt-publish` an `mqtt` one, `sftp-get` an `sftp`
one. Each new protocol is therefore a node **and** a connection type, which is a cleaner
unit of work than "add a node" and makes the env-configuration surface grow predictably.

**The connection boundary is universal, not a first-party convention.** A client-authored
component that imports `wasi:http` and takes an absolute URL in its opaque node config would
bypass the application-level model entirely — host and network policy would still impose the
outer ceiling, but the artifact would stop promoting unchanged, publish-time validation would
be incomplete, templates could not enumerate their external requirements, attribution would
break, and draft-safety rules would be evadable through component configuration.

> **Every node using an external protocol — standard or custom — declares a typed connection
> requirement and receives the resolved connection through its host capability context. Raw
> environment-specific endpoint authority does not live in flow or node configuration.**

**The connection ABI belongs to 2B, not 2C** — it is the architectural boundary 2B exists to
establish, not authoring tooling. If the authoring workstream owned it, built-in nodes would
acquire one connection model while the custom-node SDK evolved another, which is precisely
the universality this rule closes:

```
2B owns   connection semantics; requirement / instance / binding;
          connection type contracts; authority and generation model;
          the host capability interface (WIT); runtime resolution and enforcement

2C owns   generated SDK bindings; authoring ergonomics; component examples
          and harnesses; manifest declarations; conformance of custom nodes
          to the 2B interface
```

A deliberately privileged **raw-egress** component may be introduced later, but it must be
*named* as an escape hatch with its own authorization, not emerge accidentally from generic
custom-node config.

**Effect policy is portable; connection authority is environmental.** The canonical
resolved-node contract carries `pure | effectful` and participates in artifact and bundle
identity. The portable connection requirement names the protocol and authority constraints;
the environment binding selects an immutable instance generation and credential. The effect
attempt records those exact facts. Remote endpoint behavior cannot strengthen the effect
policy or authorize another dispatch.

**Decision (wamn-ko5r.14): do not adopt a host-component provider for connections.** The
trusted in-process adapter already owns the typed HTTP contract, canonical authority
resolution, one-frame admitted identity, credential installation, and durable attempt
ordering. A provider would duplicate lifecycle and authority across bind/unbind state and
the call path without adding production capability, increasing caller-isolation, cleanup,
and policy-proof obligations for no production benefit. Keep `host-component-plugins`
disabled. This applies D17's in-process-host boundary; it does not amend D17.

**Decision (wamn-ko5r.2, narrowed by `wamn-0h0g.4.9`): a connection
type defines transport semantics; an instance supplies environment facts.** A connection
type contract is portable and versioned. It fixes the protocol operations and ABI, authority
and field ownership, credential injection, and target normalization. It does not assert that
a particular endpoint is safe for repetition.

The requirement / instance boundary is exact:

| Layer | Responsibility |
|---|---|
| **Resolved-node contract** | Says which connection contract the executable consumes and whether the occurrence is pure or effectful. |
| **Portable connection requirement** | Names the required type and portable authority constraints. |
| **Instance generation** | Supplies canonical endpoint, TLS/proxy posture, credential handle, and environment policy facts. |
| **Binding validation / effect attempt** | Validation matches requirement to generation; the attempt records the exact instance and credential generations authorized for the occurrence. |

For HTTP `0.1`, method names establish neither response stability nor absence of
receiver-specific effects. The adapter emits no platform-generated outbound retry header;
GET, HEAD, PUT, and DELETE all follow the same one-dispatch effect protocol.

**A release declares its connection requirements; the environment satisfies them.**
Publish-time validation becomes: every referenced connection exists in the target env, is
the right type, and its required fields are filled — failing at promotion with a precise
error rather than at first dispatch. **This is the same requirements manifest item 10's
starter applications need**, so connections are most of what makes a vertical portable.

### 2C · Node authoring as a product

**Two authoring activities, not one.** Composing a flow and writing a node are different
products with different personas, surfaces, and lifecycles — and conflating them, as this
plan previously did by burying nodes inside item 6, under-serves both.

| | Flow authoring (item 6) | Node authoring (here) |
|---|---|---|
| Who | client developers, mostly | platform operators, regularly; clients occasionally |
| Surface | studio / JSON + ctl | SDK, builder, CI |
| Skill | low-code composition | Rust or TS, WIT worlds, a build pipeline |
| Lifecycle | releases | OCI artifacts, signed and versioned |
| Cadence | daily | per capability |

**"Low-code" describes flow composition only.** Node authoring is unapologetically
pro-code, and the product framing should say so rather than blur it. The line inside flows
is already right and worth stating as policy: **expressions inline, code as components** —
JMESPath in `transform` covers reshaping; there is deliberately no arbitrary-code node.
Every competitor ships "run JavaScript"; refusing it is a change-control feature for
industrial clients, not an omission.

**The pipeline exists; the product does not.** `services/builder` already runs
refuse-on-violation stages: a dependency allowlist resolved **before** the build so an
off-policy crate's `build.rs` never runs, then `cargo build --target wasm32-wasip2` **or
`jco componentize`** for JS/TS, then import lint through `component-policy`, in a
credential-less egress-restricted sandbox, out to a signed SBOM-carrying OCI artifact.
Two languages, real supply chain. What it lacks is ergonomics, distribution, and a fast
inner loop.

**The two-speed loop is a requirement, not an accident.** Flow iteration is seconds; node
iteration is minutes (allowlist → build → lint → sign). That asymmetry prices the two
activities honestly, but it means node authoring needs its **own** inner loop — a local
harness that runs a component against fixtures without the full pipeline — or the platform
team pays the slow path dozens of times a day.

**Draft runs consume built, signed components.** There is no unsigned fast path into a
run: the supply chain must not have a hole at the moment code is least trustworthy
(item 6's loop).

### 2D · Distribution and patchability

**Distribution is unowned.** The builder emits a signed OCI artifact; nothing says how it
reaches a project's usable catalogue. Who installs what into which project; whether there is
a curated first-party set beside a per-org private set; whether one org's node may ever
reach another (a marketplace question); and what feeds the studio's node palette, which is
concrete and near.

**Digest pinning versus patchability — the sharp one.** `component_digests` are pinned in
the immutable flow artifact, which is right for reproducibility. It also means **a security
patch to a platform-authored node requires republishing every flow that uses it** — and
those are client releases the platform cannot perform.

| Option | Buys | Costs |
|---|---|---|
| Pin digest, require republish (today) | Perfect reproducibility | An unpatchable fleet in practice |
| Pin a version range, resolve at admission | Patchable | Breaks replay equality — one flow version runs different code |
| Pin digest + an audited platform override channel | Patchable, reproducible by default | An escape hatch needing serious governance |

**Not the third — not yet.** An override channel substitutes code without republishing,
which weakens the property this whole arrangement protects: the tested artifact is the
deployed artifact. There is no fleet demonstrating the need. First implementation:

```
pin digests
support emergency revocation (disable the node; dependent flows fail loudly)
automate impact discovery and republish proposals
```

Introduce substitution only when real fleet maintenance shows those three are inadequate.
`platform_revision` remains the right shape for recording what actually ran if it ever
arrives.

**Revocation scope is a semantic decision, not a mechanism detail.** "Prevents further
execution" is ambiguous across new admissions, a parked run resuming at the vulnerable node,
a run already executing it, a bundle containing the plug but not currently dispatching that
node, and a draft artifact already tested but unpublished. *Candidate:*

```
new admissions                    blocked
parked / resumed future dispatch  blocked, explicit revoked-component failure
executing bounded attempt         completes, or is handled by the runtime
                                  cancellation-authority model (deferred seizure)
draft → publish                    fail-closed under the same rule as bundle drift
all outcomes                      the exact digest is retained for audit
```

### Done when — per sub-item

**2A** — the packaging shape is chosen from measured evidence; a capability-bearing node
composes; a composed runner's imports are exactly its bundle's capability classes, proven by
inspecting the artifact; draft and published execution use the same bundle.

**2B** — the identical flow artifact runs in dev and prod against different connection
definitions; a flow referencing an unsatisfied connection fails at **publish**, not dispatch;
a destination outside the connection's authority fails at dispatch; **a connection that
cannot satisfy a declared recovery requirement rejects the artifact rather than silently
degrading it**; and **an incompatible new connection generation is refused while the current
compatible generation and bindings remain unchanged**. Disabling a binding is a separate,
explicit operator action.

**2C** — a platform operator authors a connector through the same builder pipeline a client
uses, it installs into a project, and it appears in the usable catalogue; the node-authoring
inner loop runs a component against fixtures without the full pipeline.

**2D** — the compose-in-vs-hop posture and the patchability posture are each chosen from
fleet evidence rather than anticipation.

### Failed if

Warm-instance economics do not hold — one runner per project today versus N specialized
runners — and cold-start or memory cost forces the interpreter back as the default. That
would mean least privilege stays a runtime check rather than a structural property, and the
capability story reverts to policy enforcement.

### Decision points

- **Do custom nodes compose in, or keep D7's hop?** Composing in removes a signed round
  trip and preserves memory isolation; the hop preserves independent lifecycle and a coarser
  trust boundary. It changes the supply chain: today the builder signs a node, composed you
  are also signing — or verifying the constituents of — a composition.
- **Does the interpreter survive for the draft loop?** Only if composition proves too slow —
  and H1 says the tested artifact must equal the deployed one, so a split path is a
  correctness question, not an ergonomics one.
- **Standard versus component for a given capability** — no longer a capability-union
  question; now a judgment on call frequency (per-row vs per-run), parse risk (untrusted
  bytes), and release cadence.
- **The connection field boundary** — deferred, per above.
- **Distribution model** — curated first-party plus per-org private; marketplace never, later,
  or designed for now.

### Testing

- **Pooled-instance density and hygiene (v2.6.0 pooling):** measure runs-per-process against
  the one-run-per-replica baseline; and prove hygiene as a **separate invariant from bundle
  identity** — identical bytes are no proof that invocation-scoped state was reset.
  Prove no cross-invocation leakage of credentials, egress grants, trace context, tenant or
  run identity, connection generations, config, or guest memory, plus an instance-retirement
  policy for traps, cancellation, deadline interruption, and `max_invocations`.
- **Composition:** a flow's composed component imports exactly its bundle's capability
  classes and nothing more — asserted by inspecting the artifact's imports, not by a policy
  check; two flows with the same **execution-bundle identity** resolve to one cached
  composition, while the same node-type names at different digests do **not**; **a change in
  execution-bundle identity triggers exactly one recomposition — and under capability-class
  packaging, adding a node whose plug is already present may require none**; a private component is not served from cache to an
  org not entitled to it.
- **Connections:** the identical flow artifact runs in dev and prod against different
  endpoints; a flow referencing an unsatisfied connection fails at **publish**, not at
  dispatch; a URL resolving outside the connection's authority fails at dispatch.
- **Supply chain:** an unsigned or lint-failing component cannot enter a draft run; the
  allowlist refuses before any `build.rs` executes.
- **Patchability** (testing what the plan builds, not the deferred override channel):
  revocation prevents further execution of a vulnerable digest; impact discovery identifies
  every affected published flow; republish proposals preserve tested-artifact /
  deployed-artifact identity; and every run remains attributable to the exact component
  digest it used.
- **Warm instances:** composition count, memory, and cold-start measured against the
  single-runner baseline at realistic flow counts — the failure criterion above.

---

## 3 · Measurement gate

**Boundary.** Owns the R6 verdict and the deferred constants. Does **not** own
remediation — a miss opens a Postgres investigation inside items 1 and 2, never a second
source of truth for run state.

**Goal.** Decide whether the durable-run model holds on its own budgets — before anything
else is built on it.

**Position.** After the execution model is complete, so the verdict lands on **the
architecture that ships** rather than an interim one. Measuring before item 1 measures a
telemetry default; measuring before item 2 measures a warm-instance profile that will not
exist. Baselines are taken *inside* items 1 and 2 — §17's own sequence is
baseline → change → re-measure — and this item is the verdict, not the whole measurement.

**Depends on** items 1 and 2A. **Staged, and the final stage binds** — "blocks nothing
formally" was incoherent for a gate that decides whether the durable-run architecture meets
its budgets:

```
baseline                     before item 1
checkpoint/capture compare   during item 1
bundle-specialization        during item 2A   (cache hit rate, cold start, memory,
  viability                                    latency, platform-upgrade recomposition)
final R6 + placement verdict after items 1 + 2A
```

**The final stage blocks broad investment** — the studio, templates, self-service scale —
in an architecture whose operating envelope is unknown. It does **not** block a thin
prototype: one pilot exercising one vertical is how you learn whether clients can author
anything, and that learning should not wait on a throughput verdict. The line: anything
that *assumes* the envelope waits; anything a single pilot exercises does not.

- **D3** — Postgres owns run durability: run row and queue row in one admission
  transaction, claims by `FOR UPDATE SKIP LOCKED`, executor writes fenced by the queue's
  lease generation. Core NATS carries lossy doorbells only, and no `LISTEN`/`NOTIFY`
  anywhere — which is what keeps transaction-mode pooling available later. *Revisit at
  >~1k discrete runs/sec for one project — run the tuning matrix first.*
- **D15** — every run is durable through that one transaction; the synchronous producer
  differs only in *placement* — its queue row is born claimed, executing inline — never in
  durability. **Conditional:** the mechanism binds, but the SLO numbers were recorded
  "pending explicit product sign-off" which never happened. Claim no latency credit.
- **D5** — measure against strict per-host connection-pool caps with the DB path left
  CPU-quota-uncapped. Transaction-mode pooling is the pre-authorised escalation. The
  replication connection cannot sit behind a transaction-mode pooler.

**Done when** the spec's enumerated scenarios are measured against owner-set budgets —
both respond shapes, the single-dispatch effect path, burst concurrency, and reclaim latency
after worker kill.

**Failed if** the budgets are missed after the tuning matrix is exhausted, *or* if the
measurement cannot be made deterministic enough to compare runs. **A miss opens a Postgres
investigation — never a second source of truth for run state.**

**Decision points**
- **Run placement — inline or queued.** The exit condition of this item. **Not two execution
  models** — R6 makes every run durable and the queue row exists either way, born *claimed*
  for inline or *available* for queued; admission, checkpointing, recovery and the caller
  protocol are identical. Only *who executes it* differs, and inline exists solely because a
  caller is waiting and queueing would add queue latency plus a waiter round-trip to a
  synchronous path. If measurement says that does not earn its complexity, there is one
  placement.
- **Throughput per process, not just per run.** An executor holds one `ExecutionHost` and
  drives claimed runs through `&mut` — one replica, one run at a time — so request
  concurrency currently scales only by pod count. Measure runs-per-process with and without
  2A's instance pooling; the answer governs the synchronous path under either placement.
- **Do D15's latency SLOs get product sign-off?** Until they do, the synchronous path
  carries a mechanism with no numeric commitment.
- **The deferred constants** — idempotency TTL, outcome retention, purge windows, sweep
  cadence — are set here or stay guesses.

**Item 3 measures the shipping topology 2A selects** — specialized composition if its gate
passes, the linked-runner fallback if not. **A composition miss does not falsify R6:** they
are separate verdicts over different things, and blending them would let an execution-economics
result read as a durability-model failure. Reports stay attributable rather than collapsing
into one pass/fail:

```
persistence          checkpoint · replay seed · capture · WAL and database behaviour
execution placement  selected runner topology · memory · cold start · steady-state latency
bundle production    composition or preparation cost · cache behaviour ·
                     platform-upgrade regeneration
combined             the end-to-end product path
```

**The three storage classes are measured separately** (item 1's split): active checkpoint,
replay/audit seed, and capture each have a different scaling model, and only the first is
bounded by concurrency. A single "durable storage" figure would let seed and capture growth
hide behind a healthy checkpoint result.

**Fanout scale characterization — pulled forward.** One database per `(org, project, env)`
creates a large fanout surface, and self-service provisioning (item 10) cannot be sized
without knowing it. Characterize at 100 / 1,000 / target project-environments: database and
pool counts, replication-slot and retained-WAL pressure, migration and reconciliation
duration, backup and restore cost, and the idle project floor. This is an entry gate to
item 10, not a production-readiness afterthought.

**Alternatives if budgets miss**

| Option | Buys | Costs |
|---|---|---|
| Postgres tuning (indexes, partitioning, pool shape) | Preserves one model, one recovery story | May not be enough |
| Transaction-mode pooling (D5 pre-authorised) | Connection density | Constrains session state; replication conn must bypass |
| Sweep-variant queue — no upfront row | Removes one write from admission | Trades it for sweep latency and a second recovery path |
| A second source of truth for run state | Throughput | **Rejected by the spec's exit criterion** — listed to name what we are refusing, not as an option |

---

## 4 · Security structural closes

**Boundary.** Owns the gap between the trust level the model assumes and the surface
actually shipped. Does **not** own identity or authorization (item 5) — this is structural
isolation, not who-may-do-what.

**Goal.** Close the gap between the trust level the security model assumes and the one the
shipped surface actually has.

**Moved from last position to second, and this resolves a contradiction in the previous
plan** — which stated in its risks that these closes "close before the remaining features,
not after," and then scheduled them at item 10, after everything. The custom-node path is
already live; raw SQL ships with the POC.

**Depends on** nothing. Runs in parallel throughout — but "parallel" must not mean
"indefinitely deferred while surfaces ship around it." Each close gates a specific exposure:

| Close | Blocks |
|---|---|
| Node-host concurrency (H9) + fail-closed signing | **custom-node execution**; any client surface able to invoke custom nodes; multi-author pilots where custom nodes are available. *Not* a standard-node-only surface — generic HTTP ingress carries its own auth, concurrency, body-limit and timeout gate |
| Dedicated user-SQL role | raw SQL in a pilot or the studio |
| Capability-context fencing | less-trusted multi-author execution; custom-node composition (2D) |
| Tenant-floor completion | multi-project or multi-client data exposure |
| Mutation proofs on the relevant gates (0.2) | any claim that the corresponding boundary is shipped |

**Contents**
- **H9** — `services/node-host` still awaits `serve_connection` inline inside its accept
  loop, so one slow connection stalls every other: an unauthenticated DoS on a live path.
  *Absent entirely from the previous plan.*
- **Fail-open signing** — the node host admits an unsigned invocation when no signing key
  is configured and `--require-signing-key` is unset.
- **The raw-SQL structural close** — **D8**'s stated precondition is a dedicated user-SQL
  role holding no grants on platform bookkeeping tables. It does not exist. The shipped
  guard is a blocklist that dynamic SQL can defeat.
- **Capability-context fencing** — the spec stages generation-fenced capability dispatch
  with H9 precisely because they share a runtime surface; deferred seizure is the interim
  rule that retires when this lands.
- **Per-invocation credential and egress context is safe because *wamn* serialises** —
  `NodeRuntime` locks one warm instance, the engine has one active dispatch slot per run,
  `ExecutionHost` is driven through `&mut`. None is a written invariant, and P2 does not
  impose them. Latent today, real the moment any concurrency is adopted. Make the context
  genuinely invocation-scoped first.
- **The two platform tables outside the tenant floor** — one guarded by plugin-side
  validation, one by nothing.

- **D8** — author-written SQL runs only through the parameterised raw-SQL node, behind the
  RLS floor and a per-project-env flag defaulting off. **The POC accepted enabling it ahead
  of its own precondition, by name. A real project does not.**
- **D7** — a custom node is invoked by a signed in-cluster HTTP POST from the runner to a
  node host holding its warm instance. The hop is transport only; a manifest purity
  declaration, not the transport, decides replayability.

**Done when** raw SQL executes under a role that cannot reach bookkeeping tables even
through dynamic SQL; the node host refuses unsigned invocations by default and survives an
adversarial connection suite (concurrency, slowloris, oversized bodies); capability
dispatch is generation-fenced; and every platform table is under the tenant floor or
carries a recorded exception.

**Failed if** a close requires re-architecting the runner — which would mean the trust
boundary is deeper than the isolation model assumes, and the *model* is what needs
revision, not the code.

**Decision points**
- **Does raw SQL ship to any real project before the role exists?** The POC's acceptance
  does not transfer.
- **Is per-node isolation inside the runner ever cryptographic, or does it stay logical?**
  Today the capability union is coarse; a multi-author project makes that a real boundary.

**Alternatives for the raw-SQL close**

| Option | Buys | Costs |
|---|---|---|
| Dedicated per-project user-SQL role, no bookkeeping grants (D8's precondition) | Holds under an adversarial author | Role and grant lifecycle per project env |
| Statement parsing / allowlist (what shipped) | Cheap, already built | Defeatable by dynamic SQL — not a boundary |
| Keep raw SQL POC-only until the role exists | Zero risk | Blocks a product capability indefinitely |

---

## 5 · Identity and access

**Boundary.** Owns who a principal is and at which tier. Does **not** own what they may
author (item 6) or provisioning workflow (item 10) — only the roles those consume.

**Goal.** Real callers, real users, real roles — the platform identity provider, sessions
and API keys, and role checks at both the org and project tiers.

**Moved up from fifth.** It blocks every client-facing surface, and it is what makes the
callable-flow admission path a real authentication rather than a POC key check.

**Depends on** the POC's ingress boundary. **Blocks** items 6, 9, 10.

### Identity keys — settled semantics, settled shape

**Abbreviations are the machine-facing identity; names are labels.** The same split just
adopted for nodes, one level up:

**Three concepts, previously conflated:**

| | Role | Authority? |
|---|---|---|
| **name** | display label; mutable, free charset | no |
| **abbreviation** | stable readable handle for org and project | no — readable convenience |
| **tenant key** | the registry-minted opaque authority identity | **yes** |
| **physical resource name** | domain-legal encoding for Kubernetes, Postgres, buckets | no — derived |

> **The registry mints one canonical opaque tenant identity. Each downstream naming domain
> derives a deterministic *legal* resource name from it while preserving its unique
> component.** Abbreviations may participate in those names; they are never the authority.

That last clause matters concretely: the candidate key contains underscores, so it is
**not** a legal Kubernetes DNS label as-is — "valid in every downstream naming domain" was
wrong. Each domain gets a derivation, not the raw key.

Org abbreviations are globally unique; project abbreviations are unique **within an org** —
the org segment disambiguates, so global project uniqueness would constrain clients for
nothing. A slugified default is suggested from the name at creation, the client may
override, charset and uniqueness are validated then. That makes it a concrete requirement
on item 10's project-request API.

**This collapses a real divergence.** "Project id" currently means two things: the
registry's `valid_project` — `[A-Za-z0-9_-]` up to 64 — and provisioning's K8s-friendly
lowercase slug `[a-z0-9-]`, bounded so `wamn-db-<project>` fits Postgres's 63 bytes. So
`My_Project` passes the registry and fails provisioning: two validators, one concept, two
crates — the same bug class `identifiers.rs` exists to prevent. One abbreviation with one
charset, used everywhere machine-facing, removes it.

**The tenant key is minted by the registry at project creation and never changes:**

```
acme_recv_k3m9x2p7
└──┘ └──┘ └──────┘
 org  proj  8 random [a-z0-9]
```

≤12 + 1 + ≤12 + 1 + 8 = **34 chars** against `valid_tenant`'s 64-char `[A-Za-z0-9_-]`
bound, with headroom. Unique-indexed. *(Candidate encoding — the settled architecture is
registry-minted, immutable, collision-resistant, bounded, and **deterministically
derivable into a legal name in each downstream domain** while preserving its unique
component.)*

**The key is opaque to software; its readable prefix is diagnostic only.** Nothing parses
it — the segments go stale the moment a label or abbreviation changes, and the registry
mapping is the sole authority for `tenant → (org, project)`.

**Physical resource names must carry the minted key or its unique suffix.** If a database,
bucket prefix, backup path, or Kubernetes resource is named from the reusable abbreviation
alone, the delete-and-recreate hazard the suffix exists to prevent simply reappears one
layer down.

**What the random suffix is for, since the abbreviations are already unique:** **reuse after
deletion.** Delete project `recv`, create another with the same abbreviation, and without a
suffix the new project inherits the old one's tenant key — and with it the old blob objects,
archived runs, backups, and idempotency namespace. This is exactly the hazard the flow spec
already handles one level down for attachment IDs (*"removed ID → tombstoned; no reuse — what
makes stable IDs safe as idempotency namespaces"*). The suffix buys that safety without a
permanent tombstone registry. Recording the reason matters, because otherwise someone will
later observe that the abbreviations are unique, conclude the suffix is redundant, and be
wrong.

**Why minted rather than derived.** A key derived from names re-keys every durable row when
an org or project is renamed — runs, artifacts, releases, admissions, activation, capture —
turning a cosmetic action into a data migration. Hashed derivation is dominated outright: it
needs the same reverse-lookup mapping as minting and gives up rename survival for nothing.

*Correction of record:* an earlier revision recorded `tenant = org:project` as settled. That
literal is **invalid** — `:` is not in `valid_tenant`'s charset, and two 64-char inputs
overflow the bound under any plain concatenation. The same revision claimed deriving the key
would close a `valid_tenant`-unbounded / plugin-capped-at-64 mismatch; that mismatch is
**historical**, described in `identifiers.rs` as "the pre-R16b divergence," and the current
code carries the bound. Both were mine, both were marked settled, and the semantic
decision — one registry-owned key, collision-free, length-bounded, read by every consumer —
is what actually survived.

### Topology — D6 confirmed, not amended

**One database per `(org, project, env)`.** Projects do **not** share a database, and
environments never do — dev work cannot crash prod.

*Correction of record:* an earlier revision said the event plane **forecloses** sharing
projects within an `(org, env)` database. It does not. A publication is a set of tables
*within* a database and a slot streams *from* a database — but one database can hold many
projects' tables with **per-project publications and per-project slots**, and `cdc-reader`'s
"one session for ONE project-env" would still hold. Database-scoped replication makes
sharing **more complicated, not impossible**. A trade-off was presented as an inevitability,
which is worse than getting the trade-off wrong.

**The decision stands on its actual merits:** physical project isolation; moves and restores
that are a database operation rather than an exhaustive filtered extract; independent WAL,
vacuum, and failure domains; containment for raw SQL; clearer credential boundaries.

What that buys: isolation is **physical** everywhere — project data by schema *and*
database, run plane by database. `tenant_id` is therefore constant within any project
database, so the RLS floor is **defense-in-depth** rather than the only thing between two
customers. Project moves are a database move rather than an exhaustive filtered extract.
There is no noisy-neighbour path through the pool, WAL, or autovacuum. And `raw_sql_enabled`
is contained to its own project's data — still worth item 4's dedicated role, but not a
cross-customer risk.

Costs, named honestly: **fan-out.** Every platform schema change converges across every
project database (already the model — `migrate-catalog` per project env,
`reconcile-run-plane` converging); each project database carries its own replication slot
pinning WAL independently, so the stall-and-invalidate failure the event-plane doc flags
multiplies per project and needs per-slot monitoring (item 11); and connection pools are
per database, so pool sizing does not amortize.

### Disposition role binding — decided

The v1 effect-disposition matrix in item 1 binds at the project tier: a project
deployer or admin may manually park/release in that project, while only the
project admin may resolve. Platform admin is the separately audited
break-glass and cross-project authority. Author or publisher identity grants
neither permission and cannot be used to self-resolve an attempt. This split
keeps routine recovery operations project-local while reserving an assertion
about an external outcome for the stronger role; item 1 owns the immutable
attempt-set and audit requirements.

### The identity core — first-party first, federate later

**Decision (wamn-ctc8.4, 2026-08-07): the platform owns a thin first-party identity core in
the system database; OIDC/SSO arrives later as an *additional issuer* against the same
session seam.** Not an external IdP as the initial authority, and not a rewrite when
federation lands.

The core is deliberately small and already shipped (`crates/identity/platform`): humans and
services are the same kind of thing — **principals** — local verification is Argon2id, roles
are opaque canonical slugs bound at the project tier with no permissions attached, and the
authenticated principal is a value with no public constructor and no deserialization, so no
adapter can turn a request field into trusted invocation context. It lives beside the
registry in the platform system database, so the principal store and `tenant_of()` answer
from one plane.

**Presenters resolve; they do not authenticate twice.** Personal access tokens (wamn-ctc8.7)
serve CLI and agent callers, browser sessions serve the console (wamn-ctc8.9), and both
resolve into the one principal-and-role seam above. Federation (wamn-117) maps an approved
external subject onto an existing platform principal and reuses that same seam, revocation,
and audit — an added issuer, not a second authority.

**Ruled out.** No-auth and shared-token stopgaps: they make the admission ledger's principal
digest a fiction, and nothing that a client already depends on comes back out. Keycloak, Ory,
or a hosted IdP as the *initial* authority: it buys enterprise SSO that no client-POC surface
needs and costs the org- and project-tier role model that every one of them does. JWT by
default as a substitute for the seam: a bearer format is not an identity model, and
self-describing tokens weaken exactly the revocation the disposition matrix above assumes.

**Three identities that must not merge.** *Management identity* — the principal calling the
management surface, authorized by org- and project-tier roles. *Per-project application
identity* — `app_system`'s users, roles, and api_keys inside one project's own database,
serving that client's end users. *Workload identity* — the credentials a running flow
presents to a connection instance, owned by item 2B's environment policy. None of the three
authorizes another.

**Agents are ordinary service-principal clients.** An agent authenticates as a service
principal and holds roles like any other caller; a link to an external session log travels as
command metadata for provenance, never as authority.

**Inbound key material and caller policy live in the system database**, beside the principal
store. An auth source still names a credential-set handle and defines policy rather than
carrying material; the identity core is what owns the material behind the handle and the
policy deciding which callers it admits.

The Studio authoring specification is maintained outside this repository, so its open IdP
entry (O8) closes on the spec side when this posture is applied in v0.16 (wamn-jvzx.7).

### Still open here

- **Org-tier roles.** A client admin who requests a project is **org-scoped** — above any
  project, and the role cannot live inside a project that does not exist yet. But
  `docs/archive/schema/app-schema.md` ships `users`, `roles`, `permissions`, `api_keys` **per project**.
  The org tier is a genuine gap, and item 10's project-request API depends on it.
- ~~**Build or adopt the IdP** (Keycloak / Ory / hosted vs. our own).~~ — **Settled
  (wamn-ctc8.4):** neither. The first-party core above is the initial authority, presented by
  PATs (wamn-ctc8.7) and browser sessions (wamn-ctc8.9); an external IdP may federate in later
  as an additional issuer (wamn-117).
- ~~**Where inbound caller policy lives** — the auth source names a credential-set handle;
  nothing owns the material or the policy behind it.~~ — **Settled (wamn-ctc8.4):** the
  identity core in the system database owns both, beside the principal store.
- **Org creation** is a platform-administration function and is **deferred** — it leaks
  into no architectural decision here.

**Done when** a real user authenticates, holds a role at the right tier, and that role gates
an admission; the admission ledger's principal digest is a real identity; `tenant_of()` is
the single registry **resolution** every consumer uses — a minted key is state, not an
algorithm, and is not derivable from `(org, project)` without the registry; and a signing
key rotates without publishing a release.

**Failed if** the org tier cannot be modelled without restructuring the per-project auth
schema already shipped — which would mean identity needs one model spanning both tiers
rather than two.

---

## 6 · The authoring product

**Boundary.** Owns the loop, the surfaces, and what an author sees. Does **not** own the
execution model it builds against (items 1, 2) or the node catalogue's growth (item 2).

**Goal.** A client designs a schema, authors a flow, configures its exposure, and sees what
happened when it ran — without hand-writing JSON or touching `ctl`.

**New item.** The POC proves the first capability rung *mechanically*; nothing in the
previous plan makes it **usable**. Verified at `7ea1fb2`: no management or studio crate, no
studio document in a 30-document corpus, and no item owning it. For an opinionated low-code
platform this is not a surface among others — it is the product.

**Depends on** item 2A (the execution model the loop runs against). Item 5 gates
client-facing use and retained client identity, but the next-wave internal draft-run proof
may use the existing development administrator; it is not client exposure. **Also 2B, but
only for the complete acceptance journey** — the worked example resolves a connection named
`erp`, so an outbound-integration vertical needs connections; management API and basic
editing can begin before. Given the industrial positioning, the first vertical almost
certainly has outbound integration, so treat 2B as required for 6A's *acceptance*, not its
*start*. The studio's dependency on item 9 is a decision point below.

**2A/6A seam.** 6A builds ABI-facing against the frozen `wamn:node@0.1.x`
contract and treats linked versus composed execution as an implementation choice until
2A's exit decisions close.

**Split, because three deliverables should not share one completion gate:**

| | Scope |
|---|---|
| **6A** minimum authoring loop | definition read/write path, structured validation, draft run, public versioned authoring projections, and a headless checkout client sufficient to prove the first loop |
| **6B** complete studio | canvas, palette, exposure and credential UX, uniform platform/client authoring, template export |

**Decided (wamn-ftfc.1): one transport-neutral application model beneath every front
end.** The canonical 6A model is a transport-neutral application layer of typed per-use-case
requests and results whose handlers perform validation, authorization, optimistic lifecycle
transition, and audit attribution once; Git/CI, the org-scoped management API, and CLI
commands are adapters that supply authenticated principal and provenance to those handlers,
while `wamn-ctl`'s platform-operator and recovery effects remain outside the authoring model.
The current tool is already classified as a native operator tool, and its Clap root dispatches
directly to one-shot verbs (`architecture/package-roles.json:104-119`;
`services/ctl/src/main.rs:21-98`; accepted disposition `docs/archive/findings.md:4189` and
`docs/archive/findings.md:4218`).

This is a semantic boundary, not a package or process decision. A CLI adapter may remain in
the existing binary, but it receives the same application-scoped authority and invokes the
same handler as the API; it does not turn a Clap argument type, file path, stdout contract,
superuser connection, or cross-system operator effect into the product model. Item 6A's
handlers preserve the existing PostgreSQL authorities rather than adding a command journal.
The Git-path decision and item 5 still own how a verified commit identity maps to principal
versus attribution — Git provenance never bypasses handler authorization.

Making either the current `wamn-ctl` modules or the management HTTP schema canonical is
rejected: the first leaks operator privilege and effect-shell concerns into an org-scoped
surface, while the second makes transport own application semantics. A whole desired-state
reconciler is also rejected as the umbrella model because draft execution, suite execution,
audit reconstruction, Replay, Run again, and disable are distinct use cases; a definition
command may still reconcile desired state behind its application handler.

**Binding authoring doctrine (2026-08-07): editor is a client role, not a platform
component.** The platform ships the authoring surface: `wamn-ftfc.1`'s canonical commands,
`wamn-ftfc.2`'s Git-backed definition write adapter, and public versioned projections. Our
SPA, alternative frontends, and a human or agent working in an IDE through checkout files
plus CLI/API are substitutable clients of that surface. No frontend receives a private
command, projection, credential, or platform-plane authority.

**Studio hosting is therefore a deferred 6B client decision, not a 6A dependency.** Where a
reference shell is served and which service owns it folds into `wamn-b454.1`; the deferred
shell is `wamn-b454.5`. The iteration-loop wave ships no shell. `wamn-ftfc.13` first
enumerates the frontend-neutral editor contract; `wamn-ftfc.14` supplies the headless
checkout CLI; `wamn-ma5` publishes the stable-node/stable-edge suite and coverage projection
and proves the loop without frontend code. The Studio overlay (`wamn-b454.4`) later renders
that same projection rather than defining another model.
The `.13` contract is the pure `wamn-authoring-model` package plus its generated public JSON
Schema and normative authoring-surface documentation; it defines data, not a transport or
frontend runtime.

**Four parts**
- **a. The management plane** — an org-scoped API over schema, flows, attachments,
  activation, and credentials. Application-plane, not platform-plane: it is scoped to one
  org and must never require multi-org privilege.
- **b. The studio** — schema designer, flow canvas, exposure configuration, credential
  management.
- **c. Author-facing run visibility** — run history, per-node execution detail, failure
  surfacing, read-only audit reconstruction, and effect disposition. *The previous plan
  carried "run-status surface scope" as an open question; post-POC it is this item's core,
  because a post-release failure is
  invisible to the caller by construction and SRE dashboards do not serve an author.*
  Visibility does not grant disposition authority: the item 1/item 5 matrix
  applies, including the prohibition on author/publisher self-resolution.
- **d. Node library breadth** — the error and retry semantics an author can reason about,
  and the palette the studio presents. **Node *authoring* moved to item 2**: composing
  flows and writing components are different products, and conflating them under-served
  both.
- **e. The project-request route** — the client-admin surface for "give me a project,"
  fronting item 10's provisioning workflow. Org-scoped, so it depends on item 5's org tier.

**Required adapter (`wamn-ftfc.2`) — Git as the definition write path.** Clients edit and
push files; Git authentication supplies the adapter principal and commit metadata supplies
`changed_by` provenance; the shared handler still authorizes every command. Git is input and
attribution, never an authority bypass. This shrinks the management plane's definition side
while retaining its versioned command/projection surface and fast lever (disable). For a
human or agent in an IDE, the files are the editor; `wamn-ftfc.2` is consequently
wave-critical rather than an optional Studio choice.

### The uniform-authoring bar (owner decision)

**Everything is authored the same way — client applications and platform-operator
applications alike.** There is no privileged authoring path and no simplified subset with
an escape hatch to hand-written JSON for the hard cases.

That is a raise on this item's scope, and it is deliberate: **the studio must be good
enough to build shippable industrial verticals in.** If the platform team ever needs a
private tool to author a starter application, the claim has failed and the platform has two
divergent authoring paths — the exact outcome the rule exists to prevent.

It also gives this item a hard acceptance test (below), and it makes the platform team a
consumer of item 10's provisioning: **the first org and project created are our own.**

### Authoring-model decisions (owner)

**Target the advanced surface. Defer the simple one.** Item 6 builds the full authoring
surface for a developer-shaped author.

The **simple surface is also an authoring surface** — the operations persona builds flows
too, just narrow ones. Its dominant case is **report generation**: a schedule, a query, a
format step, a delivery step. That shape needs a small subset of the node catalogue and a
small subset of each node's configuration, and it is plausibly the commercial wedge —
"email me a weekly stock report" is how a non-developer first touches the platform, and it
is a task an operations person can own end to end.

Deferral is safe because it is a **strict subset**: every flow the simple surface can
produce is expressible in the advanced one, over the same model, the same artifacts, the
same runtime. Advanced first means the simple surface arrives as a constrained editor, not
a second IR — no rework, and no wall where an author switches modes and cannot switch back.

The rule that keeps it a subset, and that constrains the whole config surface rather than
just a UI:

> **Every author-controlled setting exposed by the advanced surface has a safe default, so
> a flow is fully authorable while touching only the simple surface. System-derived protocol
> identity and node-contract properties are read-only or hidden.**

Capture policy and deadline tuning are author-controlled within environment and attachment
limits — defaulted, advanced-only, exactly as above. The other two are not settings at all:

- **Occurrence keys are engine-generated protocol identity.** They derive from the
  completed-visits map and identify node facts and effect-ledger rows. An author never edits
  one; it may be visible as a read-only diagnostic.
- **Effect policy is a contract property, never a free setting.** The only values are
  `pure | effectful`. A method, header, SQL claim, or checkbox cannot authorize repetition of
  a possibly completed effect. Each effectful occurrence has one immutable attempt and at
  most one dispatch.

  **Operator authority terminalizes uncertainty; it does not resolve the external outcome.**
  The run-state-owned transaction verifies `effect-uncertain`, records one immutable action,
  and fails the node and run. It never rewrites the attempt, asserts success, grants
  compensation, or silently re-executes anything. An ordinary flow author receives no
  implicit operator authority.

**Requirement this places on descriptors**, without prescribing the taxonomy: node and
connection descriptors must distinguish **who owns each field** — author-editable,
environment-owned, or system-derived — because that metadata is what lets the later simple
surface be a constrained view over the same model rather than a second IR. The exact tiers
are 6A's design.

Same shape as the env-policy rule: a surface narrows what you *can reach*, never what the
model can *express*.

**The dependency this exposes is not UI work.** A report flow needs nodes that do not
exist: formatters (CSV, XLSX, PDF) and delivery (email, SFTP). Those are **item 2's**
catalogue growth, each with its connection type. And a large report is exactly item 1's
bulk-payload case — a 200 MB CSV is a handle, not a payload. So the simple surface is
gated on capability, not on design: **items 1 and 2 must land before it is buildable at
all**, which is an argument for the deferral independent of scope.

**Concurrency: last-write-wins, for now.** Two authors editing one flow resolve by LWW.
*Future item for exploration:* notify an author when an update lands while they are
actively editing, so they know they are about to overwrite someone's work or are holding a
stale version. Cheap to state now, expensive to retrofit after there is data.

**Node ids are identity; labels are free.** The model already separates them —
`Node { id, type, label: Option<String>, … }`, with `label` marked "editor." Ids are
load-bearing well past authoring: `node_runs.node_id`, occurrence keys
(`run:node:occurrence`), engine dispatch, capture rows. So the rule must be explicit:
**an id is minted once and never changes; a label may change freely.** Nothing today
prevents a client regenerating ids on edit, and if one does, run history, capture, and
every error reference decouple from the flow.

**Validation errors need structure, not just prose.** `Issue { severity, code, path,
message }` already carries real location — `code: "double-release"`, `path:
"nodes[3].type"`, `message: "respond node \"accepted\" is reachable after caller
release"`. Three gaps for a UI consumer: `path` is a JSON-document index, not a node id
(meaningless on a canvas); relational errors (`double-release`, `region-re-entry`,
`unanswerable-path`) report one participant when the author's question is "reachable from
*where*"; and `no-response-node` reports `path: "nodes"` — the whole array — which is
honest and unhelpful. Add structured `node_id` and, where relevant, `related_node_id` /
`port`, so a UI highlights rather than parses.

**Do not foreclose a canvas.** Whether an advanced author eventually gets a visual canvas
is open; four things must be true now so the answer stays open, and only the last is
expensive to retrofit:

- **structured errors** (above) — a CLI reads prose, a canvas needs fields;
- **incremental save** — a UI saves continuously; drafts already allow this if they accept
  partial, invalid graphs;
- **palette metadata** — the node catalogue must expose display names, config schemas, and
  port declarations, not just type strings;
- **canvas layout lives outside the flow artifact.** Positions and groupings inside the
  graph JSON would mean dragging a node changes `graph_hash` — minting artifacts for
  cosmetic edits and giving two semantically identical flows different identities. A
  sidecar keyed by flow id, in the draft/management layer. **Decide this before the
  artifact model hardens**, not when a canvas appears.

### The dev loop — drafts without redeploying

The published loop is *edit → publish version → release → confirm → trigger → observe*.
Every turn mints an artifact, **a catalog release**, and an activation confirmation — and
can be bounced by the coarse promotion rule if a parked test run is still alive. A morning
of iteration produces a hundred releases; release price is being paid for artifact-level
work.

**Artifacts are cheap; releases are expensive**, and they are already separate concepts.
So:

- **Two concepts, not one.** A **draft document** is mutable, may be structurally invalid,
  and supports incremental editing — it is what a UI saves continuously. A **validated
  draft artifact** is immutable, content-addressed, runnable, and eligible for composition.
  Both are needed and the plan previously conflated them: it required incremental save of
  incomplete graphs while worked example D says an invalid graph produces no artifact. Both
  are true only with the split. Fix it before the management model hardens; the schema can
  wait.
- **Draft documents and composition-cache candidates use a version-independent
  `DraftContentHash`; executable drafts do not.** The document/cache key excludes the
  proposed publish version, so changing only that proposal does not duplicate mutable
  authoring state or composition work. A validated runnable draft nevertheless pins the
  exact proposed runtime/publish version and the ordinary `Artifact` hash (whose identity
  includes that version), together with the execution-bundle hash. Publication must reuse
  that exact version/artifact/bundle tuple; selecting another version requires revalidation.
  This distinction preserves H1 instead of treating a version-independent cache key as an
  executable identity. The stored suite's source version remains independent: it selects
  the immutable suite and binding base, never executable draft membership.
- **A validated draft artifact pins the execution bundle, not only the graph.** Otherwise
  "publishing never changes the executable" is false: a draft tested under platform revision
  P1, with platform nodes updated to P2 before publish, produces identical flow bytes running
  a different executable. The draft must pin or resolve immutably to the exact bundle
  identity later promoted — graph identity, resolved node interfaces, component digests,
  platform/runner revision, adapter and world versions. **Promotion fails closed.** If any of `component digest`, `runner/platform revision`,
  `interface/WIT version`, `adapter set`, or `revocation state` differs between the
  successful draft run and publication, publishing either **uses the exact tested bundle**
  or **refuses and requires another validated draft run against the replacement**. There is
  no "acknowledge the warning and publish the new resolution" — that path silently breaks
  the invariant the pinning exists to protect.

  The same rule crosses environments: promotion carries the pinned bundle, so the target
  environment must support it **as-is**, or promotion requires a target-revision validation
  run rather than silent recomposition.

  **Accepted cost, and it couples to 2A:** a platform upgrade invalidates every bundle, so
  every in-flight draft goes stale on upgrade and must be re-run before publishing. That is
  the price of the guarantee, not a defect to design around — but it also constrains cache
  eviction, since a bundle referenced by an unpublished draft cannot be collected while that
  draft remains publishable.
- **The run is ordinary and durable.** It pins the applied catalog version (the schema it
  runs against) and the draft artifact hash, so it is reproducible, and the tested bytes
  are byte-identical to what publishing deploys.
- **Exposure reuses the authoring attachment** — pre-confirmed, dev only. The invocation names
  the validated draft identity directly instead of resolving executable membership through
  `release_flows`. Draft lineage is a closed source pair:
  `runs.trigger_source = 'scenario-draft'` and
  `invocation_context.source.producer = 'draft-scenario'`. Either draft marker without its
  matching counterpart refuses before artifact load; arbitrary non-draft producer strings
  cannot grant draft access. When neither draft marker is present, the existing heterogeneous
  release producers retain the release-membership path — `.11` does not enumerate or narrow
  every legacy release source pair.
- **No activation confirmation**, because nothing about the release changed.

**Decision (wamn-ftfc.6): 6A's fast loop is flow-draft only.** The workspace used by 6A and
`wamn-ftfc.11` contains a mutable flow document, its validated artifact, and stored-suite
and report state, all evaluated against exactly one applied catalog version and the existing
environment attachments and connections. It has no provisional schema, connection,
attachment, or project-definition world. An applied-catalog change requires revalidation
and another run against the new applied version; schema, connection, and attachment edits
continue through ordinary dev releases. Acceptance requires schema design to be possible,
not fast. A full project-definition draft workspace that tests those definitions together
remains a recorded candidate for later, not a dependency of 6A or `wamn-ftfc.11`.

The steady-state loop becomes *edit → validate (~ms) → run → observe*, with composition
(item 2A) invisible because it is keyed by execution-bundle identity and cached — an author waits only when
using a node type new to the environment.

**Decision (wamn-ftfc.4): draft retention is reachability-first, not a shared TTL.** 6A
publishes the authoritative roots for active draft-document heads and validated draft
artifacts that remain publishable. A retained draft run or stored-suite report is an
independent root for its exact draft artifact, execution bundle, applied catalog identity,
and capture and payload objects; superseding or deleting the draft document does not weaken
that root. Expiry or deletion first removes publishability, after which GC may collect only
objects with no remaining root. 2A may evict only unrooted execution-bundle cache entries.
Published artifacts are outside draft GC and remain governed by their published/run
retention. The internal author-loop proof retains all drafts, reports, and their reachable
objects and runs no automatic expiry or sweeper: it records reachability without inventing
a TTL, schema, or cache algorithm.

**Publishing then changes identity and reachability, never the executable**: same composed
runner, same capabilities, same interpreter. That preserves H1 — the tested artifact is the
deployed artifact — which is exactly what rules out "interpret in dev, compose in prod."

#### Worked example A — an ordinary edit (the 95% case)

An author is building the receipt flow. `{transform, conditional, postgres, respond}` is
already composed in this environment.

*Illustrative — no timing below is measured; item 3 sets the real numbers.*

```
edit        change the hold-threshold expression
validate    §3 predicates pass                        pure, negligible
hash        new bytes → new draft artifact
bundle      unchanged → composition cache hit         no work
draft-run   authoring attachment, dev env, real dev Postgres
observe     2 holds created; per-node emissions in the run view
```

**Nothing was deployed.** No released flow version or release membership was minted; there
was no activation or composition. The validated executable still carries the exact proposed
runtime/publish version that publication must reuse. The author sees a durable run in history
flagged as a draft, pinned to the draft artifact hash and the applied catalog version. Twenty
of these before lunch cost twenty immutable validated-draft rows and twenty runs.

#### Worked example B — adding a capability (the edit that should feel consequential)

Same author adds an `http-request` node to notify an ERP system.

```
edit        add http-request, reference connection "erp"
validate    §3 passes; connection "erp" exists in dev
hash        new draft artifact
bundle      CHANGED — this execution bundle is new to the env
compose     WAC-link the bundle → push to dev registry    the only new cost
draft-run   the composed runner imports wasi:http +
            wamn:postgres — and nothing else
observe     ERP call visible; response body in the run view
```

**Whether that pause is tolerable is exactly what item 2A measures.** If composition
latency or cache hit rate misses, this example is wrong and the loop needs a different
shape.

The pause lands on the edit that **widens the flow's capability set**, which is the right
place for it to be felt. Every subsequent edit — including by a different author
in a different project using the same five node types — is back to example A, because the
composition is cached by execution-bundle identity.

The composed component **cannot** open a socket, touch a filesystem, or publish to a
broker. Not refused at runtime: those imports do not exist.

#### Worked example C — publish and promote

The author is satisfied and publishes.

```
publish     draft bytes → flow_artifacts v4 (versioned)
            release transaction under the head lock
            attachment "receipts-http" confirmed (dev auto)
live        POST /receipts reachable in dev
```

**No build. No composition.** The composed runner has already executed this exact bundle
dozens of times. Publishing minted an identity and opened a door.

Promotion to prod carries **the same `flow_artifacts` v4 bytes** — same graph hash, same
execution bundle, same composed runner. What differs is entirely outside the artifact:

| | dev | prod |
|---|---|---|
| connection `erp` | `https://erp-sandbox.example.com` | `https://erp.example.com` |
| credential `erp-api` | sandbox key | production key, rotated independently |
| attachment limits | generous | real deadlines |
| activation | auto-confirmed (studio) | explicitly confirmed, audited |
| capture | `full` | `off` |

The artifact that ran in dev is the artifact that runs in prod. That is the property the
whole arrangement exists to protect.

#### Worked example D — the loop when something is wrong

```
edit        wire conditional.false to a second respond
validate    FAIL  double-release
                  node: respond-b, reachable from: accepted
—           draft DOCUMENT saved; no validated artifact, no run, no composition
```

Validation runs before any **executable artifact** is stored, composed, or run — the invalid
draft *document* stays saved for continued editing. A broken graph never reaches a validated
artifact. **Whether that error carries `node` and `port` context is the difference
between a usable loop and a guessing game** — and it is the first thing the walked artifact
will hit (see the validation-error gap below).

A second failure mode, once a run exists:

```
draft-run   parked on an http retry from the last attempt
publish     BOUNCED — coarse rule: a nonterminal run is pinned
                      to the applied release
—           dev env policy auto-cancels it and retries   (auto_cancel_on_publish)
```

In prod that bounce is correct and the operator waits or cancels deliberately. In dev the
env policy absorbs it — one of the three flags that may narrow but never widen.

### Env policy: may narrow, never widen

**The sandbox is tightest where code is least reviewed.** Dev is the highest-iteration,
least-reviewed, least-understood-blast-radius environment, so the industry default
(permissive dev, locked prod) is backwards here. The governing rule:

> **The environment establishes an upper capability boundary; a flow may narrow within it.
> Environment bindings may legitimately differ laterally.**

The earlier form — "the environment may only narrow" — was neat and false. Dev and prod are
not globally ordered by authority: a sandbox ERP endpoint and a production ERP endpoint are
*different* authorities, not narrower and wider versions of one thing. What must hold is
that the flow cannot exceed its environment's ceiling; environments themselves differ
laterally.

| Flag | dev | prod | Widens? |
|---|---|---|---|
| `draft_runs_enabled` | yes | no | **yes — an admission capability** (see below) |
| `auto_confirm_activation` | **authoring attachment only** | no | yes if unscoped — must not cover http/internal/cron |
| `auto_cancel_on_publish` | yes | no | no — cancels your own parked runs |
| admitted capture | `full` for a direct draft-run | `off` | immutable run policy; not portable artifact data |

**Draft execution is a capability, not merely a path.** A draft run mutates the development
database, uses development credentials, calls permitted external systems, and consumes
durable-run resources — the worked example makes a real ERP call and writes real rows. So:

> Draft execution is a **dev-only, explicitly authorized admission capability**. It may use
> only the environment's approved connections and capabilities, under real execution limits.

**Decision (wamn-ftfc.5): draft-safe connection authority is explicit, generation-scoped,
and default-deny.** The exact immutable connection-instance generation resolved for a draft
effect requires a revocable `draft-safe` grant from an environment administrator authorized
for that connection. The grant is environment policy, never portable artifact data: it is
not inferred from an environment name or hostname, cannot be set by an author, and is never
inherited by a successor generation. Draft admission checks every currently resolved
generation, and the effect path resolves and rechecks the exact generation before any effect
or network access. A missing or revoked grant refuses before access. Author confirmation may
add friction or narrow use within an existing grant, but it can never create or widen one.
For the internal author-loop proof, the existing development administrator may install the
grant; item 5 still gates retained client identity and client-facing exposure.

And the two execution modes must not become interchangeable product concepts:

| | What it is |
|---|---|
| **Scenario execution** | doubles, controlled fixtures, deterministic observation |
| **Draft execution** | real dev-environment effects through an unpublished artifact |

Resource limits (dispatch budgets, deadlines, fuel, memory) protect against **author
error**, which is a dev phenomenon — so the authoring attachment carries real limits, arguably
stricter than production's. The temptation runs the other way and is worth resisting
explicitly.

**Done when** a client with no repository access builds the receiving flow end to end
through the UI, exposes it over HTTP, and diagnoses a deliberately failing run from the
interface alone.

**The template dogfood is a joint 6B/10 gate, not item 6's.** Authoring the receiving
vertical in the studio and exporting it as a template requires template *instantiation*,
which is item 10 — the previous wording created a dependency cycle. The uniform-authoring
bar still holds; its proof lands where both halves exist.

**Failed if** the studio requires platform-plane privilege to do its job — which would mean
the application-plane / platform-plane split is wrong, and the management API's whole shape
needs revisiting.

**Decision points**
- **Does the studio dogfood the platform** (a wamn application built on the generated API)
  **or ship as a privileged native app?** This decides whether item 6 depends on item 9 or
  precedes it, where the reference shell is served and by whom, and remains the largest
  sequencing question in the document. It is deferred and does not block the headless 6A
  exit proof.
- **Flow canvas: build or adopt.**
- **Does authoring write flow JSON directly, or a higher-level model that compiles to it?**
  The second is a second IR, and the spec's whole validation story is over the first.
- ~~**How much of the run-visibility surface is API vs. UI?**~~ **Settled
  (wamn-ftfc.7): API/application read model first.** The canonical typed application handler
  and query are the source of truth. `wamn-ftfc.11` provides durable, read-only lookup by
  report identity with suite/case pass-fail, draft-vs-release lineage, exact draft-artifact
  and applied-catalog IDs, linked run and failure detail, and edit-to-run timing; it
  exposes no disposition mutation. `wamn-ma5` publishes the supported versioned projection
  keyed by stable node, branch, and edge identity, including per-node pass/fail and explicit
  branch/edge coverage state, and proves it through the headless checkout client. Public
  means a supported client contract, not unauthenticated access: until item 5, the live proof
  uses the existing development administrator and creates no retained client identity.
  Canvas rendering remains `wamn-b454.4`.

  The internal loop reserves a deterministic report identity before its first admission and
  appends immutable observed case facts. It finalizes the immutable summary only from all
  expected facts, or from an explicitly refused contiguous prefix. If a deterministic run
  exists without a captured fact after process loss, the same identity remains visibly
  pending as capture-interrupted: retry never reruns, resumes, fabricates, or finalizes it.
  This is the honest durability boundary; the end-to-end command path is not described as
  crash-safe.

**Alternatives**

| Option | Buys | Costs |
|---|---|---|
| Studio as a wamn application, dogfooding the platform | Strongest possible product signal; every gap the client hits, we hit first | Forces item 9 first; bootstrapping pain; slowest path |
| Privileged native SPA over the management API | Ships sooner; no bootstrap ordering | Does not dogfood; risks two divergent authoring paths |
| `ctl` + JSON only until pilot demand proves otherwise | Cheapest; honest about maturity | The product is then a framework, not a low-code platform — a positioning decision, not a scheduling one |

---

## 7 · Release normalization

**Boundary.** Owns what a release contains and its lifecycle. Does **not** own the
promotion *experience* (item 10) or event-consumer convergence (item 8).

**Goal.** Access policies, seed data, and event registrations become release members with
version keys; releases gain a retire and purge lifecycle.

**The 7/8 split, settled:** item 7 owns the **registration definition as a versioned release
member**, with publication and lifecycle semantics. Item 8 owns the **active registration
projection**, JetStream consumer reconciliation, the reader fleet, and replay semantics.
Previously both items claimed versioning.

**Depends on** the POC. Deliberately after the product slice, because the slice does not
need it. Bundling registrations here is a plan choice — the spec leaves the event tranche
unscheduled.

- **D24** — a release that would drop an entity still referenced by an event registration
  is refused in preflight, before any mutation, naming every orphaned registration. The
  release path never seeds or prunes registrations. **Re-opens here:** once registrations
  are release members, orphan refusal moves from the live table to the release boundary.

**Done when** a release carries policies, seeds, and registrations; promotion is proven
against the coarse rule; and retire and purge work with their reference checks.

**Failed if** the coarse promotion rule proves untenable *before* the fine-grained
replacement is ready — a single long-parked run blocking every promotion is the shape that
failure takes, and it is predictable enough to watch for.

**Decision points**
- **When does the coarse rule get replaced?** The spec's standing rule says: when a real
  drain scenario writes its acceptance test. That test is this item's tripwire.
- **The deferred constants** for retention and purge windows land here or stay guesses.

**Alternatives for promotion impact**

| Option | Buys | Costs |
|---|---|---|
| Coarse rule — no promotion while any nonterminal run is pinned | Trivial to explain and test; strictly safe | Operationally painful the first time a run parks for days |
| Fail-closed extraction from structured config | No author burden | Blind where it matters — verified: dependency scanning sees only `config.entity`, so raw SQL and custom nodes are invisible and must be treated as depending on everything |
| Declared dependencies on artifacts + residual-frontier analysis | Precise; scales | Requires an authoring surface to declare them — i.e. depends on item 6 |

---

## 8 · Event plane completion

**Causation is a proven invariant.** Every run carries `{run, root, depth}`;
the `wamn:postgres` plugin stamps a transactional `wamn.causation` logical
message onto every run-owned transaction, the CDC reader stitches it onto
that transaction's row events, and the end-to-end proof is gated
(`[EVT-CAUSATION-E2E]`) with mutation evidence on record. Event-triggered
runs carry a real root and depth; a root run is its own root at depth zero.

**Boundary.** Owns registration exposure and durable-consumer convergence. Does **not**
own the event entry type or run admission — both shipped with the POC.

**v1 goal.** Per-org accounts, reader lease election and fleet enumeration, and the durable
consumer reconciler. Controlled Replay and current-definition Reprocess retain their settled
semantics below but are parked with item 1's durable-execution tail.

**Depends on** item 7 for registration versioning.

- **D19** — row events have exactly one path: a per-project-env logical-decoding reader →
  LSN-keyed messages on a per org+env stream → the materializer, admitting through the
  common transition. The dedup identity is registration-scoped, not stream-derived.
- **D22** — one multi-tenant reader fleet leases project-envs from the system database; the
  per-env reader deployment stays the recorded escape hatch for compliance and blast-radius
  isolation. *Revisit if per-org accounts or credential tiers force per-org reader
  identities anyway — which is this item's own work, so expect to.*

**Done when — v1 floor:** registrations are release members with a reconciler converging
durable consumers to the desired set and reporting drift, and per-org accounts isolate.
**Parked tail exit, on reactivation:** historical controlled Replay works across a
registration change, and current-definition Reprocess has distinct admission and lineage.

**Failed if** the reconciler cannot converge without manual intervention on any realistic
registration edit — an external-resource reconciler that needs an operator is not a
reconciler.

**Decision points**
- ~~**Which replay?**~~ **Settled by wamn-4u7p.1; implementation deferred 2026-08-04.**
  Historical execution under the pinned registration, release, artifact, and bundle is
  controlled Replay (`wamn-v21a.1`). Processing retained event input under the current active
  registration and flow release is the separate production operation **Reprocess** / live
  re-execution (`wamn-v21a.2`). Both protocols are parked behind item 1's reactivation
  condition; on resumption item 8 owns their ordering, idempotency, audit, and compensation
  details and does not collapse them behind one verb.
- **Per-org reader identity** — D22's own revisit trigger, expected to fire here.
- ~~**Claim-check / payload store** for oversize events~~ — **Deferred (2026-08-04):** item
  8 inherits item 1's platform-store, namespace, and GC decisions when the parked payload
  tail reactivates.

---

## 9 · Consumption surfaces

**Boundary.** Owns what the client's *own users* touch. Does **not** own the authoring
surfaces (item 6), which are the client developer's.

**Goal.** Generated API beyond CRUD, SDK and schema generation, and frontend hosting. *(The
builder's untrusted source path is node authoring and belongs to 2C.)*

**Depends on:** **9A first cut** — item 5, covering schema, routes, and the input contracts
available today. **Complete callable-contract SDK** — additionally item 7's output/error
contract surface. **9B** — 9A. Possibly **blocks item 6** — see item 6's studio decision
point.

**This is three items wearing one label.** The builder's untrusted source path belongs
wholly to item 2C — it is node authoring, not consumption. What remains splits:

| | Scope | Note |
|---|---|---|
| **9A** generated API + typed SDK | catalog-derived beyond CRUD, typegen from schema + input-schemas + routes | **may need to move earlier** if the authoring product consumes it |
| **9B** frontend hosting | hosted SPA, serving | need not block 9A |

The studio consuming the *management* API (item 6's own deliverable) rather than the
generated *data* API is what keeps this from cycling — see item 6's studio decision point.

- **D2** — the generated API stays a per-project wasm gateway compiling catalog-derived,
  injection-safe SQL. *Revisit only if beyond-CRUD work stalls far enough that an
  off-the-shelf REST layer buys more than it costs; the REST-v1 half of the original
  fallback is already spent.*
- **D10** — the platform hosts a customer-authored SPA and ships a generated typed client;
  no UI builder in the product. The "reserve empty layout tables from day one" hedge has
  lapsed, so a later builder pays a catalog migration and a release-member design.

**Done when — per sub-item.** **9A:** a client consumes their schema and callable contracts
through the generated SDK without hand-written HTTP glue. **9B:** a customer SPA is deployed,
authenticated, versioned, and rollback-capable through the hosting surface. *(Shipping a
custom node from source belongs to 2C and is no longer part of this item.)*

**Failed if** beyond-CRUD generation stalls — D2's own revisit trigger — or if the
generated client's ergonomics are bad enough that clients hand-write HTTP anyway.

**Decision points**
- **D2's revisit.**
- **Internal ordering** — the studio's dependency (item 6) may pull the API and SDK
  forward and push frontend hosting back.

**Alternatives for the API layer**

| Option | Buys | Costs |
|---|---|---|
| Per-project wasm gateway (D2, chosen) | Catalog-derived, injection-safe by construction, no extra runtime | All beyond-CRUD capability is ours to build |
| Off-the-shelf REST layer (PostgREST-class) | Immediate breadth | A second runtime, a second security model, and the RLS floor to re-prove |
| GraphQL-first | Client-shaped queries | Query-cost control and depth limiting become our problem |

---

## 10 · Operator journey and project lifecycle

**Boundary.** Owns provisioning, promotion, templates, and teardown as experiences. Does
**not** own org creation (platform administration, deferred) or the release transaction
itself (item 7).

**Goal.** Provisioning, promotion, clone and move as an operable experience rather than
`ctl` surgery — and project creation as a **client-admin request**, not an operator action.

**New item.** Previously implicit under D18 inside the callable-flow item, which spent the
decision but left the surface unowned.

**Depends on**, distinguished because they differ:

```
project-lifecycle framework   5 + 7 + item 3's fanout characterization
template framework            5 + 7
first useful industrial       additionally 2B (connection requirements)
  starter vertical            and 2C (its node bundle must be installable)
```

- **D18** — an environment is a validated slug resolving an org-scoped policy row, never an
  enum or a CHECK literal; the cluster is derived by one rule from placement plus policy.
  Deploy, promote, clone and move stay one `copy(src → dst)` whose cutover refuses until
  quiesce and verify are durably recorded. *A region axis is an additive column.*
- **D6** — Postgres hosted in-cluster by the operator, one database per `(org, project,
  env)` (confirmed, item 5), org clusters sized by env policy, WAL and PITR to object
  storage plus per-project-env logical dumps. *Revisit when operating Postgres ourselves
  proves a real burden at customer scale.*

### The mechanism exists; the surface does not

`crates/control/provision` is already a pure core — SQL builders, the per-project credential
Secret renderer, the connection-URL composer, the org `Cluster` SET renderer and the
per-project-env CNPG `Database` CR renderer — with effects in the `provision-project` /
`provision-org` / `provision-project-env` ctl subcommands and a `provisionbench` gate
driving the whole path against a real cluster. There is a `state.rs`.

So the honest framing is: **the low-level provisioning mechanisms exist; the durable,
client-owned control loop does not.** That loop is not a thin surface gap — it needs durable
lifecycle and reconciliation, idempotent retries, failure visibility, quotas, safe
deprovisioning and restoration, and authorization above the project tier (item 5's org
roles). Calling it "a surface, a lifecycle, and a template mechanism" understated it.

**Provisioning is inherently asynchronous**, so the API must not pretend otherwise: a CNPG
`Database` CR hands work to Kubernetes and waits for reconciliation, with slot creation,
credential rendering and schema apply behind it. The request returns *accepted*. That
requires a durable **project lifecycle** — `requested → provisioning → active → suspended →
deleting` — with failures visible and resumable rather than a half-provisioned project
nobody can see. Check `state.rs` before designing this twice.

**Org creation stays a platform-administration function** and is deferred (owner decision);
it constrains nothing here.

**Quotas get their first job at creation.** "Provisioned per the org's configuration" means
org config decides how many projects and environments, at what tier and placement — so a
slice of item 11's quota work is pulled forward to gate provisioning.

**Deprovisioning is unowned.** The inverse path — drop the databases, the slot, the CNPG
CRs, the credentials — with a soft-delete window, because a client admin deleting a
production project by mistake must be recoverable. The crate has `backup.rs`, `dump.rs`,
`restore.rs`; teardown does not appear among the subcommands.

### Starter applications — templates as instantiable releases (owner decision)

A starter application is **a release artifact not bound to a project**: catalog, flows,
attachments, seed data, applied as a fresh project's first release. That inherits the whole
release model — immutable, versioned, content-hashed — for free.

**Initial mode: instantiate a copy and retain template identity and version provenance.** A
project starts from a template and takes on its own development lifecycle immediately.
Calling that "linked" would imply a continuing management relationship this plan explicitly
defers — with no upgrade or reconciliation semantics, it is a copy with provenance.

*Future candidate:* a linked or managed mode — including the **"static application"** shape,
upgradeable with sharply limited customization — carrying extension points and upgrade
reconciliation. Same artifact, a stronger binding; not now.

**What that requires now, and it is one field:** record **template identity and version on
the instantiated release** from the first day, even where divergence is unconstrained.
Provenance is cheap while the first release is being written and expensive to reconstruct
for a project that has drifted for a year. It is what keeps the managed mode reachable.

**Drift detection is *possible*, not free.** Immutable content hashes make per-member
comparison mechanically available, but a useful answer needs semantics that do not exist
yet: parameter substitution, connection requirements, external secrets, and members the
template *intends* to vary all produce hash differences that are not drift. **Retain
provenance and requirements now; defer linked/managed upgrade behaviour** until those
semantics are defined. Extension points — declared regions a downstream project may modify, with
everything else reconciled on upgrade — can later be layered as *policy over* that
computation rather than a change to it. Naming the concept now keeps the release model from
foreclosing it.

**Portability is the real work.** A template is a release **plus a requirements manifest
plus a parameter surface** — Helm-chart-shaped, and worth naming that way before it is
invented twice:

| Release member | Portability question |
|---|---|
| `catalog_id` / `catalog_version` | remapped at instantiation |
| `tenant_id` | **minted and resolved at the target** by the registry, never carried and never derived |
| credential references | the template **declares requirements**; the target supplies material |
| attachment routes | parameterised, or fixed by the template |
| connection requirements | the template declares typed connection requirements (2B); the target supplies endpoints and credentials. Flow-level narrowing is optional, and platform host / cluster network ceilings stay outside the template entirely |
| component OCI digests | must be pullable from the target's registry |
| seed data | template content or excluded — needs an explicit answer |
| event registrations | port as members once they join the release |

**Two things, not one.** The **template framework** — export, instantiate, record
provenance — needs only 5 and 7. A **first useful starter vertical** additionally needs 2B's
connection-requirement semantics (a template declares what endpoints it expects) and 2C's
distribution (its node bundle must be installable in the target project). Conflating them
would make the framework look blocked when it is not.

**Templates are how verticals ship.** D14 keeps the core catalog ontology-neutral with
lot/serial, asset and historian models as swappable optional modules — a starter
application is the delivery vehicle for exactly those. And under the uniform-authoring bar
(item 6), they are authored in the studio like any client application.

**Done when** a client admin requests a project and it provisions to `active` without an
operator; a project environment is promoted dev→prod and cloned without hand-run `ctl`, with
the cutover gate observable; a template instantiates into a fresh project carrying its
provenance; and a deleted project is recoverable inside its soft-delete window.

**Failed if** `copy()`'s quiesce-and-verify cutover proves unusable at realistic customer
data sizes — the shape of that failure is an unacceptable freeze window. Or if provisioning
cannot be made resumable, leaving half-provisioned projects that need operator repair —
which would defeat the self-service premise.

**Decision points**
- **Self-service or operator-mediated provisioning** — a pricing and support-model
  commitment as much as a technical one.
- **Does per-project backup and restore become a customer-facing surface**, or stay an
  operator action?
- **Are templates authored in a normal project and exported, or authored separately?**
  Uniform authoring says the former; confirm it survives contact with the requirements
  manifest.
- **Seed data in or out of a template** — in makes a starter application immediately
  useful; out avoids shipping someone else's rows into a production project.

---

## 11 · Production readiness

**Boundary.** Owns posture, drills, and quotas at scale. Does **not** own the isolation
mechanisms themselves (items 4, 5) — only proving they hold under load and failure.

**Goal.** Disaster-recovery drills, quotas, and the operational posture decisions.

**Blocked on** the production posture questions in §Open — each a specified question with a
deliberately absent answer.

- **D13** — traces, logs and metrics stay in three stores behind one collector; per-tenant
  query scoping, retention tiers and sink HA build on that split. *The revisit window
  closes at GA.*
- **D16** — a component's linear-memory budget is enforced per store below a fixed platform
  ceiling. Over-ceiling is a hard store-creation error, never a silent clamp; no budget
  means no limiter.

**Done when** a restore drill from object storage meets stated recovery objectives, quotas
enforce under abuse, and every posture question below has an answer with an artifact.

**Failed if** recovery objectives cannot be met under the chosen tier structure — which
would reopen D6's topology rather than this item.

---

## Beyond

Order driven by pilot demand, not by this document.

- **D14** — the core catalog stays ontology-neutral: entities, fields, types, relations and
  constraints only. Lot/serial, asset and historian models ship as swappable optional
  modules. *Also a standing guard on every catalog change before then.*
- **D11** — before the industrial tranche, choose Timescale-in-project-Postgres versus a
  separate time-series store. The answer now depends on whether the catalog gains a float
  type, and on whether high-rate tag tables are excluded from the CDC publication.
- **D12** — decide whether we host a per-tenant MQTT broker or connect only to the
  customer's. A pricing and credential-lifecycle commitment, not a capability. *Item 2
  should avoid foreclosing a per-tenant ingress on the org account model.*

Also here: the UI builder, the on-prem profile, and frozen-flow compilation.

- **D1** *(now item 2's central decision — listed here only because Beyond holds the
  decision archive; its "opt-in backend" framing is what item 2 revisits)* — a flow
  executes by interpreting its artifact's flow JSON as IR; standard nodes
  native in the runner, custom nodes a separate component over a hop. Whole-flow
  compilation is a later opt-in backend, never the v1 path.
- **D9 / D20** — ordering is a runner policy declared on the flow; a partitioned key holds
  while its head is unavailable, with `leapfrog` the explicit opt-in to overtaking.
- **D17** — capabilities stay in-process host plugins; component invocation stays standard
  HTTP plus a Kubernetes Service or a host typed-func. *Revisit on the first real
  multi-region, residency, edge, or per-capability-scaling need — then a per-capability
  migration, not a rewrite.*
- **D21** — new platform services ship as component Service workloads; a native deployable
  requires a recorded exception, enforced by the conformance gate.

**Retired.** D4 (outbox events) was superseded by D19. Its one surviving clause — no
`LISTEN`/`NOTIFY` anywhere — now rides D3.

---

## Open decisions

Each blocks something. An entry leaves by becoming a decision with an artifact.

| Question | Blocks | Item |
|---|---|---|
| ~~How does the tenant key relate to `(org, project, env)`?~~ | **Settled (item 5):** registry-minted `<org-abbrev>_<project-abbrev>_<8 random>`, ≤34 chars, never changes; abbreviations are the machine-facing id, names are labels. D6's one-database-per-`(org, project, env)` confirmed on isolation and operational merits — *not*, as previously recorded, because logical replication forecloses sharing | — |
| ~~Abbreviation charset, length, and who picks it~~ | **Answered in item 5's body:** `[a-z0-9]`, bounded, org globally unique and project unique within org, slugified default at creation with client override. It also collapses the registry/provisioning validator divergence | — |
| **Revocation scope** | "Prevents further execution" is ambiguous across new admissions, resumed parked runs, in-flight attempts, and tested-but-unpublished drafts | 2D |
| ~~Effect retry across a connection-instance change~~ | **Settled (wamn-ko5r.1, narrowed by wamn-0h0g.4.9):** an effect attempt never dispatches twice, so it never retargets. The attempt retains its immutable connection and credential generation facts for audit; a later distinct occurrence resolves the then-active compatible generation. | — |
| ~~What a connection type's contract asserts~~ | **Settled (wamn-ko5r.2, narrowed by wamn-0h0g.4.9):** the portable type contract defines ABI, authority and field ownership, credential injection, and target normalization. It carries no endpoint-behavior claim that can strengthen dispatch. HTTP `0.1` emits no platform-generated outbound retry header. | — |
| ~~Generation activation compatibility~~ | **Settled (wamn-ko5r.3):** stage an immutable candidate and validate its exact type/contract, required fields, canonical authorities, TLS/redirect/proxy posture, credential kind, both outer policy ceilings, and every active binding's portable requirement in one per-instance serialized snapshot. An instance with no active bindings may activate after intrinsic and outer-ceiling checks. Compare-and-swap activates only while the active pointer and every validated input remain current. Any incompatibility or stale snapshot refuses the candidate, preserves the existing active generation and bindings, and requires explicit operator action to disable a binding or create another instance. Dispatch still rechecks current policy. | — |
| ~~Can endpoint-behavior evidence strengthen effect dispatch?~~ | **Settled (wamn-ko5r.4, superseded by wamn-0h0g.4.9):** no. Remote behavior is not durable platform authority; it cannot add a send or alter the one-dispatch protocol. | — |
| ~~Who resolves an uncertain effect?~~ | **Settled by the MVP cut:** nobody asserts the external outcome. A run-state-owned operator transaction verifies `effect-uncertain`, appends one immutable action with basis/evidence/correlation/principal, and terminally fails the node and run. There is no success assertion or bulk surface. | — |
| ~~What "Replay" means to an author~~ | **Settled (wamn-4u7p.1):** audit reconstruction is read-only and creates no run; Replay is exact-definition execution in a fail-closed scenario sandbox; Run again/Reprocess is fresh production admission under current definitions and authority. Retained bytes never grant execution permission, and typed lineage distinguishes the two executing operations | — |
| **Field-ownership metadata in node and connection descriptors** | The simple surface can only be a constrained view if descriptors say who owns each field — author, environment, or system. Tiers are 6A's design | 6A, 2B |
| **Do composition economics hold?** | 2A's exit; if not, least privilege stays intra-runner (code-enforced) rather than structural, and 6A builds against the linked runner | 2A |
| ~~Effect policy for standard nodes~~ | **Settled (wamn-4u7p.2, narrowed by wamn-0h0g.4.9):** standard descriptors and custom manifests resolve through one versioned contract whose only effect-policy values are `pure | effectful`; complete descriptors enter artifact identity and runtime tables may not reclassify them. | — |
| ~~Fanout after a nondeterministic producer~~ | **Not a gate:** the engine cannot fan out (one port per emission, one edge per port), so the hazard needs a feature that does not exist. Recovery must stay fan-out-addable | — |
| ~~Node ABI: implement 0.1's existing P2 streaming contract, or introduce a P3-native 0.2?~~ | **Settled (wamn-4u7p.3):** activate `wamn:node@0.1.x`'s existing P2 `streamed(payload-ref)` + `payloads` contract. The opaque reference already moves storage identity without bytes; the pinned fork's P3 cross-store bridge is a backpressured element pump, not zero-copy object-handle relocation. Defer the breaking 0.2/WASI 0.3 migration to `wamn-72i` | — |
| ~~The canonical command model beneath git and the management API~~ | **Settled (wamn-ftfc.1):** typed transport-neutral application handlers own validation, authorization, optimistic lifecycle transition, and audit attribution; Git/CI, management API, and CLI are adapters, while privileged `wamn-ctl` operator/recovery effects remain outside the authoring model | — |
| **Org-tier roles** | A client admin who requests a project is org-scoped, but the shipped auth schema is per-project; item 10's request API depends on this | 5 |
| **Template binding: what does "linked" enforce?** | Extension points and upgrade reconciliation — blocks **only the future managed/static mode**, not item 10's first implementation, which is an instantiated copy with provenance | later |
| ~~Is composition the default, or an opt-in backend?~~ | **Direction, gated by 2A:** execution-bundle specialization is the intended default because it makes capability narrowing structural — the ABI is frozen and the composed arm is gated (`nodebench`, `flow-composed.wasm`). It becomes *committed* when packaging granularity is chosen and the economics hold | 2A |
| **Exact-node or capability-class specialization?** | **The load-bearing ambiguity.** Under class packaging a plug may carry unused implementations, adding a node inside a present plug may need no recomposition, and growth inside a plug has blast radius for every bundle using it. 2A must be able to *distinguish* the two, not just measure one | 2A |
| **Packaging granularity and capability worlds** | The plug boundary is the fault-isolation, patch-blast-radius and revocation unit, not just a cache key; which worlds are composition targets | 2A |
| ~~Draft retention and cache eviction~~ | **Settled (wamn-ftfc.4):** 6A publishes authoritative draft and retained-run/report roots over the exact artifact, bundle, catalog, captures, and payloads; expiry or deletion removes publishability before zero-root GC; 2A evicts only unrooted bundle-cache entries; published artifacts are outside draft GC. The internal proof retains all draft/report state with no automatic expiry or sweeper | — |
| ~~Which connections are draft-safe?~~ | **Settled (wamn-ftfc.5):** draft access is default-deny and requires a revocable environment-admin grant on the exact immutable connection-instance generation, checked at admission and again before effect or network access. The grant is environment policy, not portable or author-settable, and is neither inferred nor inherited; author confirmation can narrow but never grant. The internal proof may use the existing development administrator while item 5 gates client exposure | — |
| ~~Flow-draft loop only, or a project-definition draft workspace?~~ | **Settled (wamn-ftfc.6):** the 6A/`wamn-ftfc.11` workspace holds a mutable flow document, validated artifact, and suite/report state against one applied catalog version and existing environment attachments/connections; it has no provisional schema, connection, attachment, or project world. A catalog change requires revalidation and another run; schema/connection/attachment edits use ordinary dev releases. A full project-definition draft workspace remains a later candidate, not a dependency | — |
| ~~Historical replay or current-definition replay?~~ | **Settled by wamn-4u7p.1; implementation deferred 2026-08-04:** historical pinned execution is controlled Replay (`wamn-v21a.1`); current-definition production processing is Reprocess/live re-execution (`wamn-v21a.2`). Both protocols resume behind item 1's reactivation condition | — |
| **Do custom nodes compose in, or keep D7's signed hop?** | Removes a round trip and preserves memory isolation, but moves the supply-chain signature to the composition | 2D |
| **Node digest pinning versus patchability** | A platform node's security patch would otherwise need every client flow republished | 2D |
| **Node distribution: curated, private, marketplace?** | Feeds the studio palette and decides whether clients can share nodes | 2C |
| ~~Git as the definition write path?~~ | **Settled by the 2026-08-07 authoring doctrine:** files plus the Git adapter are the definition-write surface for IDE/human/agent clients; `wamn-ftfc.2` ships the adapter through the canonical command model. Git supplies provenance, never authority | — |
| **Does the studio dogfood the platform, or ship privileged?** | Whether item 6B depends on item 9 or precedes it, including where the deferred reference shell is served and by whom; it does not block 6A's headless proof | 6B |
| ~~Which persona does the authoring surface target?~~ | **Settled (item 6):** the advanced surface now. The simple surface is *also* authoring — an operations persona building report flows (schedule → query → format → deliver) — deferred as a strict subset, and gated on items 1 and 2 for the formatter/delivery nodes and bulk-payload handles | — |
| **Does an advanced author eventually get a visual canvas?** | Kept open by four don't-foreclose items; only layout-outside-the-artifact is expensive to retrofit | 6 |
| **Concurrent-edit notification** | LWW settled; warning an author mid-edit that someone else saved is the recorded future exploration | 6 |
| ~~Does the E1–E11 roadmap survive, and in what form?~~ | **Settled (0.1, 2026-07-31):** it does not survive as ordering — the backlog was re-baselined onto items 0–11 as `[PLAN-*]` epics (bd sweep `wamn-role`): survivors re-anchored, speculation closed with supersession reasons, `platform-plan.md` retained as the D-number and E-heading archive of record | — |
| **Run placement — inline or queued** | Item 3's exit. One execution model, two placements — the queue row exists either way; only who executes it differs. If inline does not earn its complexity, there is one placement | 3 |
| **Runs-per-process density** | One replica drives one run at a time today, so request concurrency scales only by pod count; 2A's instance pooling is the mechanism, measured in 3 | 2A, 3 |
| ~~**Payload inline threshold and hard ceiling**~~ | **Deferred (2026-08-04):** the v1 floor is bounded in-band payloads on frozen `wamn:node@0.1.x`; the two numbers are set by measurement when item 1's parked tail reactivates | — |
| ~~**Does capture serve the studio's run view, or move to the telemetry pipeline?**~~ | **Settled (2026-08-04; carrier ratified by `wamn-0h0g.8.14`):** capture remains platform data serving the author's run history. Its sole effective run carrier is `wamn_run.runs.capture_mode text NOT NULL DEFAULT 'off' CHECK (capture_mode IN ('full','off'))`, written once by run-state admission and read by asynchronous execution. A cross-column constraint permits `full` only for draft-sourced runs; published HTTP/event and all test-set admissions are `off`, and non-draft admission paths accept no mode. The draft-run operation schema fills an omitted value as `full`; the storage default remains fail-closed `off`. The admission-pin immutability trigger protects the column. There is no `invocation_context` entry, derivation, contract change, or identity/version change. Relocation waits for the parked tail and must preserve durable Replay seeds | — |
| **Where does raw SQL's structural close land?** | D8's precondition does not exist; the shipped guard is defeatable by dynamic SQL | 4 |
| **Do D15's latency SLOs get product sign-off?** | Recorded pending it; never obtained. Until then the synchronous path carries no numeric commitment | 3 |
| **Does the catalog get a float field type?** | Industrial telemetry is natively float; D11/D12 assume it. Today authors invent a `numeric` scale or hide it in untyped `json`. The recorded ban covers material quantities only | Beyond, but decided earlier |
| ~~Run-visibility surface scope~~ | **Settled (wamn-ftfc.7 + 2026-08-07 authoring doctrine):** the typed application read model lands first; `wamn-ftfc.11` owns durable report lookup, and `wamn-ma5` publishes the supported versioned stable-node/stable-edge suite and coverage projection plus the headless CI proof. Public means client-contract stability; item 5 still gates retained client identity and live client exposure. Studio overlays consume this projection later | — |
| Output and error schemas in the release | Contract surface | 7 |
| Waiter transport | The response path stays provisional | 3 |
| The deferred constants — idempotency TTL, outcome retention, schema limits, budgets, purge windows, sweep cadence | | 3, 7 |
| Custom-node compatibility model | The node manifest declares a contract that is shape-validated and compared to nothing; an envelope change has already shipped | 2C |
| ~~Payload store — platform namespace, GC policy, client-node split~~ | **Deferred (2026-08-04):** parked with the durable-execution tail; the two-GC-mechanism and three-retention-class constraints in item 1 remain the design envelope on reactivation | — |
| ~~Per-project authoring directionality~~ | **Settled (wamn-ftfc.15):** per-project-env `authoring-mode ∈ {git-led, studio-led}`, default git-led. The mode governs only the automated sync direction — git-led admits asynchronous Git ingestion (`wamn-ftfc.16`) and no authoritative export; studio-led admits at most an export-only Git mirror and refuses ingestion. Direct authenticated command submissions are always allowed in both modes; platform validated/promoted state stays execution authority (Git supplies provenance, never authority). Enforcement lives in the canonical command handlers; mode transitions are project-admin, atomic, and audited through the authoring command ledger; mode-mismatched sync refuses typed. Blocking pre-receive hooks and simultaneous bidirectional write are rejected. Gates only git-led sync/enforcement work, never checkout-file submission | — |
| ~~Per-principal drafts and shared visibility~~ | **Settled (wamn-ftfc.17):** each draft carries an immutable denormalized owner principal; a principal may hold unlimited parallel drafts per flow (freeform draft id); only the owner mutates, with the uniform authorization-denied refusal otherwise; project-author/project-admin may view all project drafts; editing another's draft is an explicit create-successor command copying content into a new draft owned by the editor with predecessor draft-id + revision lineage metadata, source untouched; lineage is metadata and never roots retention — a deleted predecessor leaves a dangling reference rendered unavailable. Every mutation is audited with its principal. Locks, leases, and agent-specific draft machinery are rejected; agents are ordinary service principals and external session-log links are metadata only. `wamn-ftfc.18` implements | — |
| ~~Exact uint64 wire representation for JavaScript clients~~ | **Settled (wamn-ftfc.21):** the normative wire domain for `format: uint64` fields is `[0, 2^53-1]` — the schema carries `maximum` 9007199254740991, the Rust boundary refuses values above it in both directions, and the TypeScript client's `Number.isSafeInteger` fail-closed check is the final contract, not a stopgap. `2^53` and `u64::MAX` refuse deterministically; storage is already `bigint` (i64) and every uint64 field is a server-assigned counter or latency | — |
| Production posture — topology, cardinality, recovery objectives, availability, threat model, isolation, credential lifecycle, residency, audit access, data lifecycle, upgrade and client compatibility, unit economics | | 11 |

---

## Known gaps

Assumed to exist by work already in flight, owned by no document and no item above.

- **Two "project id" validators, one concept.** The registry's `valid_project` allows
  `[A-Za-z0-9_-]` up to 64; provisioning requires a lowercase K8s slug `[a-z0-9-]` short
  enough for `wamn-db-<project>` to fit Postgres's 63 bytes. `My_Project` passes one and
  fails the other — the same bug class `identifiers.rs` exists to prevent, reappearing
  across crates. *Item 5's abbreviation collapses it; the finding stands regardless.*
- ~~**Auth-source policy**~~ — the constraint is stated (sources define policy, never
  material) but nothing named where key material lives, or who owns inbound caller policy.
  *Adopted by item 5 (wamn-ctc8.4): the first-party identity core in the system database owns
  the material and the inbound caller policy.*
- **Deprovisioning.** `crates/control/provision` ships `backup.rs`, `dump.rs` and
  `restore.rs`; teardown appears among no subcommand. A client admin deleting a production
  project by mistake must be recoverable. *Item 10 should adopt this.*
- **The idempotency-key namespace** — one flat unique index per tenant, no reserved
  prefixes. Cron, event, and HTTP identities share it by convention.
- **Platform revision** — the column exists and admission rejects an empty value, but every
  row reads `legacy`: no producer gives it meaning, and it pins execution behavior.
- **Outbound endpoints live in the flow artifact.** `http-request` takes `url` in node
  config; artifacts are env-independent by key, so a per-env endpoint forces different
  bytes and therefore a different flow version — dev tests what prod does not run.
  *Item 2B owns this via connections.*
- **Two platform tables sit outside the tenant floor** — one protected by plugin-side
  validation, one by neither that nor RLS. *Item 4 should adopt these.*

---

## Risks

1. **The security model assumes a trust level the roadmap removes.** Isolation holds only
   because every statement, credential, and invocation originates from trusted platform
   code. That ends when raw SQL or multi-author flows ship — and the custom-node path is
   **already live**, with a fail-open signing default. **Item 4 owns this, and its position
   in this plan reflects that.**
2. **Some evidence of record cannot fail.** Owned by 0.2. Until it closes, every "Done
   when" in this document is a claim rather than a check.
3. **Runner capability-union coarseness** — per-node isolation inside the runner is
   logical, not cryptographic. Becomes a real boundary the moment one project has two
   authors.
4. **Capture volume** — direct draft-runs default to bounded scrub-redacted `full`; published
   and test-set runs are `off`. Node I/O snapshots remain the largest author-history storage
   driver, but capture is not recovery authority. **Item 1 owns this**; item 3 measures the
   result.
5. **Per-customer resource floor** — a dedicated org's Postgres does not scale to zero.
6. **Proof burden versus product maturity.** The callable-flow spec reached rev 18 through
   eleven adversarial review rounds; the POC plan reached r6. That rigor is warranted for
   the execution model — concurrency and crash windows do not negotiate — and item 1
   inherits it, since payload durability is the same class of problem. But the same burden
   applied to the authoring surface would starve the product. **Items 6 and 9 should ship
   on lighter evidence than the execution-model work did**, and saying so here is the only
   thing preventing the pattern from repeating by default.
7. **This map is a snapshot, not a mechanism.** Three of twenty-four decision readings were
   overturned on challenge in a single pass, and the previous revision of this document
   carried four claims that had gone false under a stale pin. Nothing here detects the next
   drift; only re-checking against code does.

---

## Two epistemic notes

**Review coverage is not asymptotic.** Independent passes over the same material keep
finding non-overlapping problems. Treat any "reviewed" claim as a sample, not a sweep.

**Counts are not findings.** Unlabeled and parentless backlog records are frequently valid
history — the head of that list is top-level epics. A backlog size is a scope, never a
verdict.
