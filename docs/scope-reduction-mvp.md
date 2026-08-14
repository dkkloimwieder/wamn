# wamn — scope reduction to MVP (rev 9)

verified-against: `e5de4356d9c827f86264270b34957194b8d9669e` · 2026-08-09
errata folded through round 10

> **Execution amendments (owner-ratified 2026-08-10, branch `mvp`,
> tracker `wamn-0h0g`).** **Versioning is pinned at 0.1 for the whole
> program**: every Cargo package version, WIT package version,
> wire-contract/schema-version literal, and versioned artifact label is
> 0.1 until MVP completes (greenfield — no compatibility risk).
> Wherever this document says contract/schema/world "0.2", read: the
> revised contract, shipped versioned 0.1. Wherever it says "0.1 is
> refused", read: the prior contract surface is deleted wholesale in
> the same change — no prior-version acceptance path exists and no
> version numeral ever bumps. Pin issue: `wamn-0h0g.10.7`. Further:
> all other docs are archived under `docs/archive/` except the
> owner-ratified root amendments named here (this charter remains the
> base live document, `wamn-0h0g.12.9`); all build artifacts were
> purged from the repo and host Docker (`wamn-0h0g.1.5`); the full
> execution rulings (1–8) live in the tracker notes.

> **Plane-residency amendment (owner-ratified by
> `wamn-0h0g.13.39`, 2026-08-14).** Read cut 5's ownership,
> publication, and retention rules and cut 4's execution-plan storage
> through `docs/plane-amendment.md`. Portable authoring, report,
> evidence, release, and execution-plan objects live in the control
> database; a project database retains only the deployed runtime
> projection, environment bindings and activation, run state, and
> application data. The amendment's convergent A/B/C publication and
> post-lease verified plan fetch supersede this document's historical
> one-project-transaction publication description.

> **Capture carrier amendment (owner-ratified by `wamn-0h0g.8.14`,
> implemented by `wamn-0h0g.8.3`).** Effective run capture has exactly
> one carrier: `wamn_run.runs.capture_mode text NOT NULL DEFAULT 'off'
> CHECK (capture_mode IN ('full','off'))`. Run-state admission writes it
> once; asynchronous execution reads it. Only draft-sourced runs may carry
> `full`; published HTTP/event and test-set admissions are `off`, and
> non-draft admission paths accept no mode. The draft-run operation fills an
> omitted mode as `full`, while the column default remains fail-closed `off`.
> The admission immutability trigger protects the column. There is no
> `invocation_context` entry, derivation, duplicate carrier, contract change,
> identity change, or version change. Oversized captured output is derived
> from stored facts: `output IS NULL AND output_size IS NOT NULL` renders
> typed `output-too-large` metadata; the read side never consults the
> write-time ceiling.

**Principle.** Product-thesis properties are mandatory acceptance
*outcomes*; no supporting mechanism is exempt, and an implementation
survives only as the smallest coherent way to satisfy its outcome. Sunk
cost creates no support obligation. Owner-ratified outcomes are closed
to relitigation absent new evidence. Every "landed" claim carries a
`file:line` or commit citation; a claim without one is a target, stated
as such. **Retained code must name its outcome — "landed" is not a
rationale.** The MVP schema of record equals the MVP feature allowlist.
This document is self-contained; **every count is derived from a table
in this document** — no free-standing numerals. Appendix F is the
justification ledger: every retained root names its outcome.

## Outcomes

| Required outcome (owner-ratified) | Minimum mechanism |
|---|---|
| Publish test gate | test-set/report/artifact exact binding, pass/fail enforced, unconditional; evidence durable with the release |
| Event spine | one seeded registration, one CDC→run path |
| Wake-from-zero | a scaled-to-zero run-worker wakes and completes queued work |
| Management surface | the exact eight-operation allowlist (contract 0.2, cut 6) |
| Tenant isolation | negative tests over MVP-exposed surfaces |
| Author-supplied SQL | the `wamn_app` role floor (below) |
| Author-supplied schema | catalog-driven additive evolution; one bootstrap proof |
| Flow composition | synchronous `call-flow`; validation-time expansion into one authoritative `ExecutionPlanV2`; callee pinned into the plan identity |
| Observability | W3C traceparent end-to-end · `wamn:logging` · `wasi-otel` export; OTLP collector is an optional external prerequisite (absent ⇒ no-op). Target work — traceparent lands only into admission today (`run-state/sql.rs:137,374`); proved by the M0 trace check. No bundled Grafana/Loki. |
| Fork currency | prompt assessment + batched adoption (below) |

## Allowlist

Triggers: request · event. **Executable node inventory (verbatim;
target = the current eleven-entry registry minus `cron` and
`time-shift` — `standard-nodes/src/lib.rs:78-92,115-128`):** request ·
event · fail · transform · conditional · http-request · postgres ·
postgres-query (**this is the D8 flag-gated raw-SQL node, default
OFF**) · respond. **Authoring-reserved:** `call-flow` — accepted by
flow-schema 0.2, **absent from runtime dispatch** (it compiles away at
validation, cut 4).

**Guarantees not made:** no cross-run scheduling or completion-order
promise; within-run graph and port semantics remain deterministic.
**Runs are single-shot** — no mid-run checkpoint/resume exists.

Unavailable (validation, registration, `publish-catalog`, and `ctl`
inputs hard-refuse): cron · timers/delay · durable child-run
invocation and parent waiting (synchronous `call-flow` within one
single-shot root execution **is supported**) · partial rerun · custom
nodes · component composition · ordering policies · automatic crash
resume · caller cancellation · flow-declared credentials and
allowed-hosts (connection-derived authority only) · automatic
idle-down · destructive schema migration · synchronous HTTP invocation
of a scaled-to-zero run-worker. A refused capability has **no shipping
code path, no wire-contract leg, and no live schema** (appendix F.2).

## Cuts

### 1 · Composition arm: deleted
The wac composed arm is the deleted custom-node plane's ABI
(`flow-driver` + `node-rs` + `wac plug`); it deletes with the plane;
`callable-flow-wave1`/`wave2` archive; no composition smoke exists.
Flow-to-flow composition is `call-flow` (cut 4) — a different,
retained thing. Future component composition is a fresh design.

### 2 · Proofs

Dispositions over the 57 gate Jobs at the pinned SHA (counts derived
from appendix D): **7 absorbed · 3 change-triggered · 0
characterization · 47 archived.** Characterization is a defined,
empty tier: manual-only; a job gains a schedule only while a named
open decision, listed beside it, consumes its result.

Blocking checks (count derived from this table: **16 named checks; 4
cluster jobs + 2 repo-local jobs**):

| Job | Named checks |
|---|---|
| M0-gate | 1 author authorization · 2 binding + evidence row · 3 HTTP connection confinement · 4 one-dispatch uncertainty + single-shot reclaim (the four crash cases below) · 5 operator terminalize · 6 tenant isolation (client surface) · 7 flow-call integration (caller → expanded callee → one real Postgres effect → result on the caller's `main`, all facts under one root run) · 8 trace propagation + export (incoming traceparent → flow-http → run → flowrunner context → standard http-request → downstream sees the same trace id; one correlated `wamn:logging` record at a narrow test OTLP sink) |
| M1-gate | 9 event causation + dedup · 10 tenant isolation (event path) |
| M2-gate | 11 wake permission + queue behavior |
| bootstrap-journey | 12 fresh provisioning · 13 p0 role probes · 14 additive migration (dry-run + apply + drift green) |
| repo-local | 15 contract-diff · 16 lint |

**RC ordering:** fresh cluster → bootstrap → M2 (before any
synchronous binding is active) → M0 (publishes and tests the binding
warm) → M1. The fork-sync subset runs on the sync change only.

### 3 · Schema of record
*(a)* No new duplicate structural representation; existing duplication
is removed when a retained change would otherwise edit both copies or
it causes a retained gate failure; live-DB drift check on the SQL
profile. *(b)* **Record equals allowlist**: the adopting change deletes
every capability without a named outcome from live schema, the
reconciler's embedded record, the drift catalog, privilege/check
inventories, builders, wire contracts, module docs, re-exports, and
owner tests — atomically. The full inventory is appendix F.2. Git
history is the archive; a family returns only with a feature that
names an outcome.

### 4 · Execution: crash floor, single path, flow calls

**Floor.** One immutable write-ahead attempt per effectful occurrence ·
at most one dispatch · **uncertainty ⇒ `effect-uncertain` and
non-claimable** · exact-generation refusal · run-status read. No
automatic successor or effect redispatch. Runs are
single-shot: the flowrunner's checkpoint/replay entry
(`flowrunner:29-42`), node-level `Parked`, and park-deadline state
delete.

**Facts, three tiers (exact):** `node_runs` is the mutable execution
projection — pre-effect rows may be replaced after lease loss; the effect
attempt/dispatch/outcome records are the **immutable effect ledger** and the
sole authority for reclaim classification (enforced by the landed immutability
trigger, `run-state.sql`); finalized test reports are **immutable copied
evidence**. Projection and ledger writes are intentionally non-atomic.

**Effect-uncertain: one durable literal.**
`runs.status = 'effect-uncertain'` **with the `run_queue` row
absent.** No generic run-level `parked` status survives. Waiting work
= a queue row whose `available_at` governs eligibility; a NULL lease
is the *claimable* state (`queue/sql.rs:188-189`). Terminal failure =
no queue row + an immutable operator action and reason. `get-run`
exposes the effect-uncertain state and its attempt facts.

**Run-state target literals (exact; residue cited in appendix A):**

```text
runs.status            dispatched | running | completed | failed |
                       infrastructure-failure | effect-uncertain
caller_outcome_kind    responded | failed

delete                 cancel_requested_kind · cancel_requested_at ·
                       cancel_kind · the runs_cancel_requested index ·
                       every 'cancelled' status/outcome/error literal ·
                       recovery-class and outbound retry-key fields ·
                       predecessor_attempt_id + legacy_imported
                       exceptions · cross-claim successor identity

effect-attempt model   pure → no effect attempt
                       effectful → one immutable write-ahead attempt
                       identity · at most one dispatch record ·
                       terminal outcome when known
```

**Delivery split:** `wamn-0h0g.4.9` lands the inaccessible run-state
primitive and its database proofs; it claims no production dispatch
activation. `wamn-0h0g.5.4` wires the private adapter and owns the integrated
attempt-before-send, one-dispatch, outcome, and pure-no-row proof.

**An effectful occurrence dispatches at most once.** A known external
failure fails the run. A sent attempt without a recorded outcome is
`effect-uncertain`; neither reclaim nor an admission retry sends it
again. The historical effect-retry state machine does not survive
under renamed fields.

**Reclaim classifier — one run-state transaction, before any guest
invocation.** Expired lease + **no effectful write-ahead intent** →
replace the abandoned pre-effect projection rows and mutable context;
re-enqueue the original admitted input for a fresh in-memory
execution. Expired lease + **any effectful intent** → remove queue
eligibility; set `effect-uncertain`; **atomically store the
caller-facing typed failure outcome** `{ code: effect-uncertain,
run_id }` whose envelope is explicitly non-committal about whether the
external effect occurred; never invoke the flowrunner. Ordinary
waiting row → execute normally. `runs.input_json` remains
authoritative until terminalization. **Inbound admission idempotency
selects the existing run; it never licenses effect redispatch.**

**M0 crash cases (four):** death before any effectful intent → fresh
re-execution · death after an effectful write-ahead attempt →
**effect-uncertain, and the synchronous caller receives the stored
typed failure** · run-worker Deployment scaled to zero distinguishable
from expired-lease reclaim · a reclaimed row never reaches the
flowrunner.

**Synchronous wait semantics (explicit, cancellation-free):** caller
disconnect **detaches and never cancels** the run · wait timeout
detaches and never cancels · re-invocation with the **same idempotency
key** returns the same run and its stored outcome (including
`effect-uncertain`) · a genuinely new invocation may repeat the
uncertain external effect — an explicit caller decision, stated.
**HTTP mapping (fixed by contract 0.2, cut 6):** any synchronous
request attachment whose expanded plan contains an effectful node
**requires an idempotency key** (validation enforces; the per-route
flag exists today as optional — `flow-http:70,238`);
`response-wait-timeout → 504` and `effect-uncertain → 502`, each with
a typed JSON body, the timeout body instructing retry with the same
key.

**Caller cancellation: deleted** — the durable protocol
(`cancellation.rs:1-5`), the dispatcher deadline sweep,
disconnect-cancel, and the wire leg (`cancel`/`CancelAck`/`Cancelled`,
invocation WIT `:78-83`). Epoch interruption is the retained hard
layer (`engine.rs:42-46`, backing `runaway-budget`).

**Single execution path.** The host-inline path deletes
(`InlineExecutionDriver`, `host.rs:24,164,209`; `execute_claimed`,
`inline_invocation.rs:95`). flow-http `begin` creates an ordinary run
+ queue row; the **warm run-worker** claims and executes; `wait` polls
the durable caller outcome. HTTP, event, draft, test-set, and
called-flow execution converge on one root-run model.

**Flow calls (`call-flow`).** An engine-reserved authoring node whose
configuration contains only `flow-id`. **Validation** resolves the
identifier through the validated draft's exact catalog ID, catalog
version, and environment to an immutable **published** callee
artifact; recursively resolves the complete acyclic call closure;
applies hard depth and expanded-node limits; **compiles the callee's
entry input schema** (the payload does not exist at validation — the
check executes at runtime, below); and **compiles the closure into one
canonical `ExecutionPlanV2`.**

**Callable eligibility (what "the call returns" means):** the callee
is a request-entry flow · every successful terminal path reaches a
`respond` · a `respond` at the call boundary has zero outgoing edges ·
success-by-frontier-exhaustion is not a callable success. The
expansion is an internal IR transformation: callee entry → a synthetic
**`call-enter`** instruction that validates the current payload
against the compiled callee input schema (failure → a typed
`invalid-input` NodeError at the call site; success → enter the
expanded callee subgraph); callee `respond` → a synthetic
**`call-return`** continuation delivering the **body** to the call
site's `main` output (the landed adapter semantics — `child_outcome`,
`flowrunner:2006`); a nested `respond` **never** releases the root
HTTP caller. An unhandled callee failure is a normal node error and
may follow the call site's `error` edge (`ERROR_PORT`,
`types.rs:24-26`). The internal call frame produces **one
author-visible fact for the call-flow node itself**, scoped internal
node facts, and a source map to original identifiers — a plan-internal
boundary, not a public node or runtime operation. **Limitation,
stated:** `call-flow` returns the callee response **body** only; the
callee's status hint does not propagate; a generic `return` contract
is a later schema change. No child run, queue row, parent wait, actor
mode, internal attachment, cancellation, lineage, or independent
recovery exists.

**`ExecutionPlanV2` — complete executable schema.** Header:
`format_version · ExecutionRuntimeRevision · root_artifact_hash`,
where `ExecutionRuntimeRevision = { flowrunner_component_digest,
effect_provider_revision, host_effect_contract_version }`. The
flowrunner digest commits to the guest engine, standard nodes, and
guest adapters — but effects execute **natively**: the executor
constructs `WamnPostgres`/`WamnCredentials`/`WamnLogging` itself and
reads the flowrunner from a file (`executor:26-28,33-35,265`), so
`effect_provider_revision` — a build-generated immutable digest over
the native effect providers, the runtime fork, and host policy — is
required for the exact-executable property to survive the deleted
identity. **At claim:** loaded flowrunner digest, loaded
effect-provider revision, and supported host-effect contract must all
equal the plan's; a mismatch refuses before guest execution.
**Upgrade rule (smallest):** one execution-runtime revision is active
in an environment; before replacing it, every active release is
revalidated, retested, and republished against the successor; old
workers drain before removal; side-by-side runtime revisions are
outside MVP (at one environment this is a bounded loop, and it is the
only rule consistent with fail-closed plan identity). Body: the entry instruction · nodes
(each: scoped path · source artifact hash · source node id · type ·
config · effect policy `pure | effectful` · resolved source
connection requirement) · edges with
source and destination ports · fan-out ordinals · root terminal
behavior · the synthetic call-enter/call-return instructions · the
compiled runtime input-schema guards · call-frame/source-map
metadata. The canonical bytes of this object **are** the
`execution_bundle_hash` preimage — one object, no parallel identity
inputs; rewiring an edge or changing a return continuation changes the
serialized plan.

**The plan is the authoritative runtime object.** Every admitted run
pins a non-null `runs.execution_bundle_hash`; effect authorization
resolves **through the plan**: hash → `ExecutionPlanV2` → node by
scoped path → source artifact + requirement → environment binding.
(Root-graph-keyed authority — `transitions.rs:116-145`;
`connection_http.rs:256,276` — is re-pointed at the plan; expanded
nodes are otherwise unauthorizable.) **Durable integrity:**

```sql
catalog.execution_bundles (
  tenant_id, execution_bundle_hash, format_version,
  exact_bytes, byte_length, created_at,
  PRIMARY KEY (tenant_id, execution_bundle_hash),
  CHECK (hash(exact_bytes) = execution_bundle_hash)
)
```

with enforced foreign keys from `validated_flow_drafts`,
`release_flow_test_evidence`, and `runs`; a missing or hash-invalid
plan **refuses admission or claim** before any guest execution.
**Sole ownership, literal:** authored graph + public flow contract →
`flow_artifacts`; the resolved executable plan → `execution_bundles`;
drafts, evidence, and runs hold the hash FK only.
`validated_flow_drafts.execution_bundle_bytes` and the artifact's
resolved-execution columns (`interface_bundle_json/hash`,
`component_digests`, `occurrence_recovery_json/hash` —
`catalog-schema:102-107`) **delete** with their registration
parameters and checks (F.2). (The
current published-release branch returns NULL bundle bytes —
`flowrunner:355-377` — insufficient once the plan carries the call
closure.) **The old artifact executable model deletes in the same
change** — `OccurrenceRecoverySelection`, `ExecutableRecoveryContract`,
connection-recovery declarations and their compatibility readers
(`catalog/model:18,50,441-459`): the artifact slims to the authored
graph + public contract; two executable descriptions would violate
cut 3(a).

**Pinning semantics.** A later callee publication does **not** mutate
or invalidate an already-validated caller bundle — immutable pinning
means drift changes what a *future* validation resolves, never a
stored identity. Revalidating the caller adopts the new callee and
mints a new plan identity requiring a new test-set report. Explicit
artifact revocation may separately refuse publication or execution.
Draft-calls-draft is outside MVP.

**Scoped identity (canonical).** Internally a structured
`execution_node_path = ["normalize", "write"]` with one canonical
serialization used for node facts, attempt uniqueness, connection
authorization, observability, and named-node assertions; flow-schema
0.2 node IDs match `^[a-z0-9-]+$`, reserving the path separator.

**Authorization and proofs.** Same environment, same validated/release
closure; the call inherits the root principal; cross-project calls and
asynchronous `start-flow` are future capabilities. Cycle refusal,
exact pinning, scoped identity, expansion determinism, eligibility
refusals, runtime input-guard behavior, and success/failure conversion
are owner-local tests; M0 carries the one integrated case (cut 2,
check 7); the existing crash cases cover a crash during a callee
effect — callee intents are root-run intents by construction.

**Operator resolution (run-state-owned).** `get-run` returns the
effect-uncertain state + attempt facts. One repair: a documented
runbook executing one run-state transaction — lock
run/node/attempt/queue → verify `effect-uncertain` → append one
immutable `operator_run_actions` row (basis, non-empty evidence
reference, correlation, prior state) → mark node + run terminally
failed → fence queue eligibility → commit. **`principal` derives from
the authenticated database identity (`session_user`) — recorded as
role attribution; individual attribution requires individual operator
roles or an authenticated proxy, and the audit row is labeled honestly
as whichever it carries.** Proved as M0 check 5. No success assertion;
no bulk selection. A replacement is an ordinary new run; no rerun
lineage is promised.

**Run admission (settled).** Authority is `wamn-run-state`: one public
admission API; raw builders `pub(crate)`; each producer composes it in
its own transaction — flow-http via the invocation provider, the
materializer inside its exactly-once event+run transaction
(`materializer/src/main.rs:49,406-408`), management via the queue
admit builders (`scenario-worker/src/lib.rs:20-23`). Management uses
the private `wamn_management_admitter` authority ratified by
`wamn-0h0g.7.5` and specified in `docs/plane-amendment.md`; it extends
this native API for stable producer identity plus draft-run `full` or
test-case `off` capture and still inserts ordinary run + queue facts
atomically. Exact retries return the existing run and different facts
refuse. Per-producer live proofs ride the slices. No admission RPC,
`SECURITY DEFINER` path, or source-string invariant.

### 5 · Publish gate

**Test-set input (inline, non-vacuous).**
`TestSetInput { definition: UTF-8 }` — the definition is
self-describing (contains its schema-version);
`TestSetIdentity { hash = sha256(exact definition bytes) }`. Document
= `schema-version · cases: 1..MAX_CASES`; case = `case-id · input ·
expect: 1..MAX_ASSERTIONS`. Zero cases or an empty expect **refuses**.
The target validated draft comes only from the `test-set-run`
operation, never from the document. Bytes are stored once, immutably:
`authoring_test_sets { tenant_id, test_set_hash, schema_version,
exact_bytes, byte_length, created_at }` with a hard size cap
(byte-storage precedent:
`validated_flow_drafts.execution_bundle_bytes/hash`,
`scenarios/catalog/src/authoring.rs:96-101`); the report holds an
enforced FK to it.

**Assertion families (exactly four; the 0.2 parser refuses anything
else, and the removed families' code and tests delete —
`DbExpect`/`Egress*`, `scenarios/model:58,63`):** run terminal
outcome · terminal respond **status/body** (the invocation envelope
carries body + optional status hint and **no headers** —
`flow-invocation:61-62`) · typed flow failure · named-node terminal
status (observed at least once; every observed occurrence matches the
expected terminal status / typed failure kind; "not observed" fails),
where the node id may be the **canonical scoped path** so the gate
reaches facts inside call bodies. Typed-output matching is
demand-gated. **Gate claim, stated exactly:** the gate proves
successful execution, request-response status/body behavior, and
named-node terminal status — nothing beyond those facts.

**Capture (two states; honest wording).** `off` = per-node status +
typed failure only. `full` = the scrub-redacted author-facing
projection up to a fixed platform maximum; above it, output is
omitted with explicit `output-too-large` metadata. Known secret
patterns are redacted; **capture is not a secret-classification
boundary** (a literal secretless guarantee is a demand-gated
allowlist-projection design and is outside MVP). Draft runs default
`full`; **published runs retain the admitted `runs.input_json` but
store no per-node input or output capture**; `test-set-run` forces
`off`. The four-mode surface and per-flow capture field delete; the
scrub pass is retained as the redaction floor.

**Durable orchestration (renamed landed machinery).**
`authoring_test_run_reservations { tenant, report_id, command hash,
validated-draft id, test-set hash, state, deadline, timestamps }` ·
`authoring_test_case_runs { tenant, report_id, ordinal, case_id,
run_id, outcome; UNIQUE (tenant, report_id, ordinal) }` ·
`authoring_test_reports` (immutable finalized summary). Invariant:
once an ordinal has a `run_id`, no retry or restart creates another
run for it. `test-set-run` returns an accepted report id; `get-report`
exposes `pending | finalized`; the management process reconciles its
own pending reservations under the whole-set deadline. Bounds:
per-case deadline · whole-set deadline · MAX_CASES · max file size;
a **deadline-exhausted or effect-uncertain case finalizes as a failed
case** and never enters operator machinery during an authoring
command. Cases execute sequentially through ordinary admission against
the warm run-worker.

**Ownership (one owner per durable lifecycle).** Test-set bytes,
reservations, case map, report → authoring (management).
`release_flow_test_evidence` **and** `catalog.execution_bundles` →
catalog/release, physically beside `release_flows`, evidence unique
per release flow. `operator_run_actions` + the terminalize transaction
→ run-state. p0 probes → the bootstrap proof path.

**Publication (one transaction).** Lock + verify the validated draft →
lock + verify the green report → verify artifact, plan identity,
catalog, environment; verify **every `source_artifact_hash` in the
plan is a member of the target catalog release and every scoped
connection requirement has a valid target-release binding**
(bindings are release-member-scoped — `catalog-schema:319`;
root-member verification alone is insufficient once callee effects
exist); verify every exercised connection generation is still the
active generation (recorded per attempt, `transitions.rs:77-108`;
**point-in-time at publication**) → create the release member →
insert the immutable evidence row `{ tenant, catalog id+version, flow
id+version, validated_draft_id, report_id (FK), test_set_hash,
artifact_hash, execution_bundle_hash → catalog.execution_bundles,
exercised_connection_generations (each binding instance_id ·
generation · definition_hash), created_at }`; **insert one
`connection_generation_retention` row per exercised generation with
`reference_kind = 'release-evidence'` and the evidence identity**
(reusing the landed mechanism — `catalog-schema:1262-1273`) → commit.
Publishing
uses the **exact tested plan** even when a newer callee has since been
released (pinning, cut 4). Retention follows release → evidence →
{ report → test-set bytes, execution bundle, **exercised connection
generations** }; run-history pruning cannot orphan publish evidence;
credential revocation stays independent — a retained definition keeps
no secret usable. The `replay-seed` reference kind deletes;
`audit-seed` deletes; no execution consumer exists;
`active-attempt` stays. **The gate is unconditional:** publication always
requires the green finalized report; the configurable registry publication
gate deletes. All new durable vocabulary is `test_*`.

**Fixtures and seeds.** The platform does not provision, reset, or
manage test fixtures at runtime. Baseline data is the landed
`publish-catalog --seed-dataset` (idempotent, deterministic ids,
`ON CONFLICT DO NOTHING` — `publish_catalog.rs:26-28,300`); cases own
per-case keys and cleanup on top. Environment data is disposable; a
full reset is the proved bootstrap loop (drop env → provision →
`publish-catalog --provision` → `--seed-dataset`). A report proves the
observed outcomes of that execution; it is not a broad determinism
claim.

### 6 · Management surface, contracts, auth, schema

**Eight operations (authoring contract 0.2, complete):**

| Operation | Input | Result |
|---|---|---|
| save-flow-draft | exact UTF-8 definition + expected revision | draft identity |
| read-draft | draft id + revision | exact stored definition |
| validate | draft revision | validated executable identity (plan expansion) |
| draft-run | validated draft + input + `capture: full\|off` | run identity |
| test-set-run | validated draft + `TestSetInput` | accepted report identity |
| publish | validated draft + successful report id | release + evidence identity |
| get-run | run id | bounded author-facing result (statuses, typed failures, effect-uncertain state, `full` outputs when captured) |
| get-report | report id | `pending \| finalized` immutable report |

0.2 ships atomically: `SuiteRun → TestSetRun`; `SuiteProjection`
removed; refusal literals renamed; the public JSON schema regenerated;
per-operation idempotency, read authorization, size limits, response
types, and typed refusals enumerated; **reads are query endpoints
outside the command ledger**; **0.1 is refused** (the ftfc.14
reference CLI regenerates). One of eight is mounted today
(`management.rs:831`) — wiring the rest **is** M0. Draft semantics are
the landed optimistic lifecycle on expected base revision (ftfc.1
fold, `36d918d`). `Grant/RevokeDraftSafeGeneration`
(`authoring/model:466,487`) **delete**: bootstrap seeds the one
draft-safe sandbox connection generation; validate/draft-run enforce
it; publish never mutates it.

**Wire contracts, all 0.2, atomic with their deletions:**
**flow-invocation 0.2** — `begin`, `wait`; outcome
`responded | failed`; the cancel leg deletes from WIT, Rust mirror,
plugin, adapter, stored vocabulary, schema, and tests; **HTTP
mapping fixed:** effectful synchronous attachments require an
idempotency key; `response-wait-timeout → 504`; `effect-uncertain →
502`; typed JSON bodies · **flow-schema 0.2** — drops the cron entry
type, ordering, partition-policy, `time-shift`, the four-mode capture
document, and **the flow-declared credential and allowed-hosts
vocabulary** (`Flow.credentials`/`CredentialRef`/`Node::credential`/
`Flow.allowed_hosts`, `flow-model/types.rs:59-70,326-328,503-508` —
unconsumed second authority vocabulary; connection requirements,
environment credentials, platform host policy, and network policy
remain the confinement story); replaces `InvokeFlowConfig` with
`CallFlowConfig { flow_id }`; node-id charset `^[a-z0-9-]+$`; JSON
Schema regenerated; 0.1 refused · **flowrunner world 0.2** — **the
product operation `run`, alone**; `check-flow` **deletes** (validation
runs through the native plan compiler); the bench/POC exports delete
(`dispatch-bench, run-next, execute-claimed, run-until-kill,
sink-count, reset, run-s6, active-version` — `world.wit:84-152`).

**Authentication (PAT-only; idempotent, wrapper-owned bootstrap).** No
`/login`, no local passwords: `authenticate_local` is consumer-less
and deletes with `identity.local_credentials`, `identity.sessions`
(`system-schema:264,346`) and the session/CSRF code
(`management.rs:15-291`). `provision-project-env` (gaining the
identity dependency `ctl` currently lacks) issues under a **stable
principal identity**. Idempotency lives in the bootstrap wrapper —
`kubectl get secret`: **exists and carries valid identity + expiry
metadata → skip; exists but invalid/expired → rotate; absent →
issue**; rotation and issuance follow **issue → verify → then revoke
the prior token**, so a failed issuance never locks the environment
out; `ctl` stays Kubernetes-client-free. The token is emitted only to
the named Secret — never logs, audit rows, or connection URLs (no
credential-bearing URLs anywhere in bootstrap output). The management
author and the M0 route caller are issued independently. The
management image ships `POST /authoring` + the eight operations.

**Author-supplied schema (additive-only).** `migrate-catalog` in the
MVP `ctl` classifies the plan **safely-additive or refuses**, applies
atomically, and drift-verifies — nothing else. Destructive planning,
`--confirm-with-backup`, `--acknowledge-impact`, and all impact
analysis (`migrate_catalog.rs:73-79,139-154`; `impact_report.rs`)
move whole to the ops feature; **no MVP impact module exists.** Ops
retains destructive diff/apply only inside restore/copy reconciliation
and impact-report planning; it exposes no destructive-migrate verb.
Legacy rollback and default-acknowledgement APIs delete. A destructive
MVP change is the proved reprovision loop.

**Attachment removal.** No fast-disable command: an operator publishes
a replacement catalog; removed attachment IDs are tombstoned
(`schema/control/src/sql.rs:225`; activation tables at
`publish_catalog.rs:468`).

### 7 · Deployment, planes, build

**Bill of materials.** Infra in-cluster: Postgres · NATS. External
prerequisites: OCI registry · OTLP collector (optional). Native
processes: management (the `wamn-scenario-worker` binary) · executor
(`wamn-run-worker`, a `[[bin]]` of `wamn-executor`) · dispatcher ·
waker · cdc-reader · wasmCloud host · wasmCloud operator (one
platform unit, two failure surfaces, counted separately). Component
workloads: flow-http · materializer. One-shot: `ctl`. Removed from
deploy: api-gateway (generated CRUD unmounted; the package deletes,
F.2), node-host + serve-node, event-reader.example, trace-relay,
surplus CronJobs, the gates image from the product path, Grafana/Loki
provisioning (`deploy/infra/grafana`, `provision-dashboards`).

**Custom-node plane: deleted wholesale, with a precise carve.**
Delete: custom-node export macros, custom WIT conversion, the no-caps
custom shell, the payload-streaming SDK surface, `services/node-host`,
`services/builder` (+ Job/netpol/signing manifests, the builder-svc
and toolchain Docker stages), `crates/platform/node-runtime`,
`crates/node/{sdk,guest,invoke,manifest}`, the `wamn:node` WIT, the
runtime plugins `wamn_node` + `node_invocation`, `ctl`
custom-publish, and every custom fixture/sample. **Move into the
flowrunner:** `CapsCtx`, PgValue ↔ WIT conversion, the trusted
HTTP-effect adapter (`node/guest/src/caps.rs`). **Pure shared types →
`flow-model::node_contract`** — the standard node interface,
capability, connection-requirement, emission/error, and collapsed
effect-policy shapes only; the custom/recovery descriptor taxonomy
dies in place. `standard-nodes` and the engine consume their shared
contract types from `flow-model` (other implementation dependencies —
entity-access, schema types, expression evaluation — unchanged).
`socketguard` remains, gating the fork's unconditional `wasi:sockets`
surface (E13). A future node SDK is a fresh design.

**Flowrunner (single-shot, world 0.2).** Executes the expanded plan
start→finish in memory; writes node facts under canonical scoped
paths; hosts the moved capability adapter; no replay, no child paths,
no node-invoke, no demo nodes; the dispatch registry is the verbatim
nine.

**Scheduler.** `Cadence` only (shared by executor,
`executor/src/lib.rs:78-256`, and dispatcher, `dispatcher:10`); the
cron module and `croner`/`chrono` delete. The dispatcher's role is
queue reconciliation, queued-work wake hints (`dispatcher:3,30,709`),
and doorbells; its cancellation sweep deletes with cancel. The
dispatcher/waker merge is declined (privilege isolation: the only
k8s-scale-privileged process holds no DB credentials, and vice
versa). M0 needs no dispatcher — the executor claims directly
(`dispatcher:80-92`).

**Execution placement.** Opaque `execution_target_id` at the
doorbell-subject + waker-mapping contract (touch points:
`executor:108-109`, the dispatcher publish, `waker:43-51`); the
placement adapter alone equates it to the tenant key; database
access, RLS, and run rows stay tenant-keyed. **Warm floor:** while any
synchronous request binding is active, the run-worker Deployment holds
replicas ≥ 1; background/event-only may be explicitly scaled to 0.
Synchronous cold-start is outside MVP (a request against a
scaled-to-zero run-worker may time out at the gateway; unsupported,
stated).

**`ctl`.** MVP verbs: provision-org · provision-project ·
provision-project-env (+ the principal/PAT/route-caller issuance
above) · enable-cdc-project-env · reconcile-replica-identity ·
reconcile-run-plane (over the cut-3 record) · publish-catalog ·
migrate-catalog (additive). Ops feature (modules
`#[cfg(feature = "ops")]`; a `wamn-ctl-ops` binary with
`required-features = ["ops"]`; ops-only dependencies regenerated at
execution after the F.2 deletions): dump-project-env ·
restore-project-env · copy-project-env · prune-run-history ·
impact-report. **`pin-run` deletes** — it writes stored suites through
the scenario catalog (`pin_run.rs:7,23,56`), incompatible with the
stored-suite deletion and inline test sets; a future
export-run-as-test-set-file utility is noted, not retained.
Assertions are executable: `wamn-ctl --help` shows no ops verbs;
`cargo tree -p wamn-ctl` shows no ops-only dependencies.

Ops persistence is one idempotent SQL artifact installed after the core
system schema: `provisioning.dumps`, dedicated
`provisioning.copy_sagas`, and append-only
`provisioning.migration_confirmations`. Core SQL has no reference to
those relations. A confirmation carries the project-env and catalog
migration identity as cross-database facts, attributes the attestation
to `session_user`, and is rechecked against the destination catalog head
by copy-project-env before execution; it is authorization evidence, not
live truth. The generated protected-write inventory marks these rows as
ops scope.

**Build graph.** One shared `cargo-chef` recipe; **package-scoped cook
stages** (`cook-run-worker: cargo chef cook … -p wamn-executor`;
likewise management, ctl, host, dispatcher, waker, cdc-reader) over
shared locked registry/target caches; each image's build stage
compiles exactly one top-level package. Component builders: M0
(flowrunner, flow-http) · M1 (materializer) · proof; wac and jco are
gone with the composed arm. **Acceptance: cook + build measured from a
clean cache, per image** (appendix E).

`deploy/mvp/` = bootstrap script + exact manifests + exact images; the
BoM is reproducible as Kubernetes resource kinds (Deployments for the
seven native processes, the Postgres resource, NATS, the bootstrap
`ctl` Job, zero CronJobs, generated Secrets/ConfigMaps recorded at
bootstrap, the external prerequisites above).

### 8 · One environment; axis kept
The environment axis stays in the data model (`EnvPolicy` as
ratified); exactly one environment exists. Environment data is
disposable; dump/restore/copy are ops verbs.

## Raw SQL floor
`wamn_app` (LOGIN, NOSUPERUSER, NOBYPASSRLS) executes author SQL —
the raw-SQL node **is** `postgres-query` (D8 flag, default OFF).
Platform bookkeeping authority is held by stable host-only NOLOGIN ACL roles.
The private management admission role is `wamn_management_admitter`
(`NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION
NOBYPASSRLS`); its scoped A/B LOGIN generations use the existing
issue-authenticate-Secret-verify-revoke lifecycle and may exercise only the
native ordinary-admission seam. It cannot mutate admitted rows afterward or
access unrelated surfaces; `wamn_app`, guests, author SQL,
`wamn_scenario_author`, and `wamn_effect_writer` remain denied.
Where PostgreSQL authentication requires LOGIN, per-environment A/B credential
generations inherit exactly one ACL role and connect only to their project
database; at steady state at most one generation is LOGIN-capable. Effective
grants and forced RLS hold the floor; the one-database-per-(org, project, env)
split confines cross-project access, and the ACL role confines in-project
bookkeeping. Author SQL has no outbound retry key or effect-redispatch
upgrade in MVP.

The protected-relation table is generated from two sources only:
`state-owners.json` supplies each installer and lifecycle owner, and a disposable
database built by the canonical reconciler supplies grants, cascades, triggers,
trigger-function owners, RLS, and constraints through `pg_catalog`. A caller above an
owner's API is a client, not another data owner. Any mutation grant to
`wamn_app` is recorded as `author SQL, RLS-bounded`. The audit changes no
production permission. All table and package identities remain `0.1`/`0.1.0`.

**p0 probe set (self-contained):** the landed role/containment
battery — effective-role identity; superuser/createdb/createrole/
replication/bypassrls flag denial; protected-relation denial
(`RUNTIME_GUEST_ROLE_PROBE_SQL`, relocating to the bootstrap proof
owner) — **extended by dynamic-SQL and qualified-identifier probes.**
Owner: bootstrap-journey (check 13); one representative case rides M0.

## Egress scope
Proofs cover the declared-connection HTTP path (authority, base-path,
address policy — landed, mutation-proofed) and the database path. No
MVP guest world imports `wasi:sockets` (verified —
`components/{ingress/flow-http,execution/flowrunner,execution/materializer}/wit/world.wit`);
the fork's unconditional socket implementation (E13) is gated by the
fork-sync subset + `socketguard` (change-triggered).

## Fork policy
Both owned forks are named: `dkkloimwieder/wasmCloud` and
`pg-walstream` (`wamn/0.8.0`). Assess each upstream minor within two
weeks; adopt on one batched monthly cadence covering both; immediately
only for applicable security fixes, support-window requirements,
confirmed blockers, or patch deletion. One dedicated sync change per
fork; the fork subset gates it; never mixed with feature work.

## MVP boundary
**M0:** flow file → save → read → validate (plan expansion) →
draft-run (capture `full`) → per-node outputs → test-set-run →
publish (+ evidence + plan bytes) → authenticated HTTP via flow-http →
warm run-worker; the flow-call case rides this slice. **M1:** seeded
registration → tenant commit → CDC → stream → materializer → one
admitted causal run. **M2:** explicit run-worker scale-to-zero →
enqueue background run → wake → complete.

---

# Appendix — exact scope

## A · CURRENT at `e5de435…` (verified, cumulative)
Workspace: 51 root members, `default-members` absent (32 `crates/*/*`,
9 `services/*`, 3 `poc/*`, 3 `test-support/*`, 4 `tests/*`); component
workspace 31 members, defaults absent. Dockerfile: 19 stages = 10
advertised image targets + 9 build stages; the shared builders compile
everything (`Dockerfile:47-49,78-81,144-161`). Gates: 60 yaml in
`deploy/gates/` (56 Jobs + 1 Deployment/Service input + 3 fixtures) +
4 JSON; one GitHub workflow
(JS client only). Key verified facts by area — management surface:
one contract command mounted (`management.rs:831`); sessions/CSRF in
the binary (`management.rs:15-291`); contract carries
Grant/Revoke (`authoring/model:466,487`). Execution: host-inline path
(`host.rs:24,164,209`; `inline_invocation.rs:6-95`); serial executor
(no per-run spawn); claim predicate (`queue/sql.rs:188-189`);
flowrunner replay entry (`flowrunner:29-42`); world bench/POC exports
(`world.wit:84-152`); published-branch NULL bundle bytes
(`flowrunner:355-377`); invocation cancel leg (WIT `:78-83`); durable
cancel protocol (`cancellation.rs:1-5`); epoch hard layer
(`engine.rs:42-46`); flow-http per-route optional idempotency
(`flow-http:70,238`). Run-state residue: 'cancelled' in status +
caller-outcome CHECKs, cancel columns + partial index
(`run-state.sql:192,213,221-223,252,282-283`); recovery-class triple
(`:402-404`); predecessor/legacy lineage (`:478-504`); effect-fact
immutability trigger (`:75-87`); legacy columns
(`:202-210,286-291,343`); effect-disposition writer-less (`:87-89`).
Ordering/partition live mechanics (`run-queue.sql:26-32,131-139`);
child machinery (`flow-model/types.rs:431-443`; `child.rs:1-6`;
`flowrunner:1530,2006,2023-2041`). Authority: root-artifact-keyed
effects (`transitions.rs:116-145`; `connection_http.rs:256,276`);
per-attempt generation identity (`transitions.rs:77-108`);
release-member-scoped bindings (`catalog-schema:319`). Catalog/model:
composition-shaped bundle identity (`:924-952,1080-1088`);
recovery-shaped executable model (`:18,50,441-459`). Flow-model:
credential/allowed-hosts vocabulary
(`types.rs:59-70,326-328,503-508`); `ERROR_PORT` (`types.rs:24-26`).
Node registry: eleven entries incl. request/event/fail;
postgres-query is the raw-SQL node
(`standard-nodes/src/lib.rs:78-92,115-128`); layering deps
(standard-nodes → node-sdk + node-manifest; engine → node-sdk;
flow-model + catalog → node-manifest); `CapsCtx`
(`node/guest/src/caps.rs`). `pin-run` writes stored suites
(`pin_run.rs:7,23,56`). Schema: reconciler embeds stored suites
(`run_plane.rs:64-67,1689`); gate policy columns
(`system-schema:172,221`; `catalog-schema:1574`); sessions/local
credentials (`system-schema:264,346`); `authenticate_local`
consumer-less; `ctl` lacks the identity dep; migrate calls impact
directly (`migrate_catalog.rs:73-79,139-154`). Observability:
traceparent lands only into admission (`run-state/sql.rs:137,374`).
Round 10: native effect-provider construction + flowrunner file load
(`executor:26-28,33-35,265`); artifact resolved-execution columns
(`catalog-schema:102-107`); `connection_generation_retention` +
kinds (`catalog-schema:1262-1273`).

## B · TARGET — workspace

**Root members: 51 → 38.** Deleted outright (13):
`crates/node/{sdk,guest,invoke,manifest}` ·
`crates/platform/node-runtime` · `services/builder` ·
`services/node-host` · `poc/*` (3) · `crates/data/api` (generated
CRUD unmounted; the entity-access kernel survives) ·
`crates/scenarios/runtime` (embedded scenario engine; management uses
ordinary admission) · `crates/scenarios/catalog` — **the
authoring/test-set store folds into the management service**
(`services/scenario-worker/src/store/`: drafts, test-set bytes,
reports; one consumer, one lifecycle; its tests run under the root
defaults), so the package deletes and the scenario terminology exits
with it.

**Component members: 31 → 6 retained:** flowrunner · flow-http ·
materializer · sockprobe · connection-http-standard · busyloop. All
others delete (F.2), including `ingress/api-gateway` and
`event-reader.example`.

**`[workspace].default-members` (19 paths):**

```toml
default-members = [
  "crates/authoring/model",
  "crates/catalog/model",
  "crates/data/entity-access",
  "crates/execution/flow-engine",
  "crates/execution/flow-invocation",
  "crates/execution/flow-model",
  "crates/execution/host",
  "crates/execution/run-state",
  "crates/execution/scheduler",
  "crates/execution/standard-nodes",
  "crates/identity/platform",
  "crates/platform/component-policy",
  "crates/platform/pg-core",
  "crates/platform/runtime",
  "crates/scenarios/model",
  "crates/schema/model",
  "services/executor",
  "services/host",
  "services/scenario-worker",
]
```

Component defaults: flowrunner · flow-http. **Profiles:** M1 adds
`crates/events/{registration,wire,materializer}`,
`services/cdc-reader`, component `materializer`. M2 adds
`services/dispatcher`, `services/waker`. Deploy adds `services/ctl`
(mvp features), `crates/control/{provision,registry}`,
`crates/identity/project-state`, `crates/schema/{control,compiler}`.
No-default additionally: `tests/*`, `test-support/*`, `ctl` ops
feature. Alias and profile lists regenerate at execution after the
F.2 deletions.

**Dependency changes (cumulative):** flow-model absorbs
`node_contract` (pure standard shapes only) and
`CallFlowConfig { flow_id }`, deletes the credential/allowed-hosts
vocabulary; standard-nodes and the engine re-point shared contract
types to flow-model (node-sdk, node-manifest delete) · flowrunner
absorbs `CapsCtx` + the WIT/HTTP adapters and executes the expanded
plan (the expander lives in validation) · host drops
`inline_invocation` + flowrunner bytes · runtime drops the
inline-driver seam and both node plugins · scheduler deletes cron +
`croner`/`chrono` · run-state deletes `child.rs`, `cancellation.rs`,
`reconstruct.rs`, `rerun.rs`, node-park machinery, the capture-mode
surface (scrub retained), the cancel columns/index/literals, the
recovery-class triple, and predecessor/legacy lineage; gains the
reclaim classifier + stored effect-uncertain caller outcome ·
catalog/model deletes the composition identity arms **and** the
recovery-shaped executable model, gains the `ExecutionPlanV2`
preimage + `catalog.execution_bundles` (FK'd from drafts, evidence,
runs; draft bytes + artifact resolved-execution columns delete) ·
scenario-worker gains the `store/` module (drafts, test-sets,
reports) · identity drops local auth · `ctl` adds identity, drops
custom-publish, dashboards, pin-run, and the scenario-catalog ops
dependency.

## C · Profile mechanism (executable)

```bash
cargo test                                   # root default (19 paths)
tools/profile m1 | m2 | deploy | full | ops  # exact -p lists from B

cargo test  --manifest-path components/Cargo.toml   # component M0 tests
cargo build --manifest-path components/Cargo.toml --target wasm32-wasip2
tools/build-components m1 | proof

cargo build -p wamn-ctl                                    # MVP verbs
cargo build -p wamn-ctl --features ops --bin wamn-ctl-ops  # ops verbs
```

Docker: `--target run-worker | management | ctl | host | dispatcher |
waker | cdc-reader` each build from their own cook+build stage;
`--target gates` requests the proof builders explicitly.

## D · Gate-manifest disposition (57 gate inputs; counts derive from these lists)
**Absorbed (7):** suiteproof, suiteexec, invocationproof, credproof →
M0 · causation-e2e → M1 · wakeproof → M2 · publish-catalog →
bootstrap-journey.
**Change-triggered (3):** egress-escape · socketguard · impactproof
(ops path).
**Characterization (0):** tier defined, empty.
**Archived (47):** apiproof, pinproof, dashproof, pocsuiteproof,
f3proof, f4proof, f2-build, f2-buildproof, f2-testgate, f2invoke,
buildproof, traceproof, callable-flow-{cron, f0, f1, f2, f3, f4,
schema, wave1, wave2}, and the 26 benches — apibench, bench,
capturebench, cdcbench, cdcbench-switchover, dispatchbench,
egressbench, failoverbench, flowbench, logbench, matbench,
metricbench, nodebench, pgbench, pgbench-multiproject, provisionbench,
queuebench, queuebench-ceiling, rie2ebench, runstate-baseline,
samplebench, streambench, testhostbench, testkitbench, tracebench,
walbench.
**Fixtures:** retained — runner-connection-egress.yaml (M0 egress),
sockprobe, connection-http-standard, busyloop (M0 runaway/epoch case);
all others delete per F.2.

## E · Images and acceptance
Product/operator images (7): host, ctl, dispatcher, run-worker,
management, cdc-reader, waker. Proof/CI (1): gates. On-demand (0).
Baseline for comparison: the 10 advertised targets, not the 19 stages.

| Measure | Current (verified) | Target |
|---|---|---|
| Root members / default-selected | 51 / 51 | 38 / 19 |
| Component members / default-selected | 31 / 31 | 6 / 2 (+1 M1) |
| Gate inputs | 57 supported | 7 absorbed · 3 triggered · 47 archived |
| Advertised image targets → product | 10 → 10 | 10 → 7 (+1 proof) |
| Long-lived: infra / native / workloads | mixed | 2 (+2 ext) / 7 / 2 |
| Root `cargo test` + `cargo check` | unmeasured | clean **and** incremental, recorded on landing |

Acceptance per image — **cook + build from a clean cache**, recorded
once on landing: run-worker `wamn-executor` only (+ closure) ·
management `wamn-scenario-worker` only · ctl `wamn-ctl` only (ops
verbs absent via `--help`; ops deps absent via `cargo tree`) · host,
dispatcher, waker, cdc-reader one each.

## F · Justification ledger

### F.1 Retained roots → named outcome
| Root(s) | Outcome |
|---|---|
| run-state (slim, + reclaim classifier + stored uncertain outcome), run-queue (global FIFO), flow-engine, flow-model (+ node_contract, `CallFlowConfig`), execution/host, runtime, executor, host svc, flowrunner (single-shot, world 0.2, expanded-plan execution, capability adapter) | crash floor · M0 execution · flow composition |
| flow-invocation 0.2 (begin/wait), flow-http | M0 authenticated admission via the warm run-worker |
| pg-core, cdc-reader, events/{registration,wire,materializer}, materializer component | event spine (causation depth = loop guard) |
| dispatcher, waker, scheduler(`Cadence`) | wake-from-zero |
| test-set model (ex scenario-model), the management store module (drafts, test-set bytes, reports — `scenario-worker/src/store/`), authoring-model | publish gate |
| catalog/model (`ExecutionPlanV2`, `execution_bundles`, slimmed artifact), schema/{model, compiler, control(slim)}, ctl mvp verbs, control/{provision, registry(slim)}, project-state | provisioning · publish · additive schema · tenant isolation (T1 minting) |
| identity/platform (PAT + OIDC seam) | management auth |
| entity-access | the Postgres standard node (`standard-nodes/src/postgres.rs`) |
| component-policy | egress confinement (import allowlist, mutation-proofed) |
| standard-nodes (the verbatim nine) | M0 node set |
| trusted context (ko5r.8), invocation_context, schema_drift | ratified call-frame context · drift check |
| traceparent path (target), wamn:logging, wasi-otel plugin | observability (M0 check 8) |
| epoch interruption + runaway-budget · scrub pass | hard kill (floor fail taxonomy) · redaction floor |
| clients/ workspace · docs/, architecture/, tools/, .beads | studio track (ratified spec; own CI) · owner process |
| tests subset backing the 16 checks + the retained fixtures (appendix D) | proof floor |

### F.2 Deletion inventory (atomic with adoption; Git is the archive)
**Host-inline execution:** InlineExecutionDriver · host flowrunner
bytes · host `execute-claimed` · inline identity/lease config · the
runtime inline-driver seam.
**Mid-run durability:** flowrunner checkpoint/replay entry ·
node-level `Parked` + transitions · park-deadline
`state_json`/`resume_at` · resume-oriented builders/docs/re-exports ·
flowrunner world bench/POC exports · `check-flow`.
**Caller cancellation (code + schema):** `cancellation.rs` ·
dispatcher deadline sweep · disconnect-cancel · the 0.1 wire leg
(cancel/CancelAck/Cancelled) · `cancel_requested_kind` ·
`cancel_requested_at` · `cancel_kind` · the `runs_cancel_requested`
index · every `'cancelled'` status/outcome/error literal
(`run-state.sql:192,213,221-223,252,282-283`).
**Effect-retry/lineage residue:** recovery-class and outbound retry-key
fields (`:402-404`; the plan retains only `pure | effectful`) ·
`predecessor_attempt_id` +
`legacy_imported` exceptions · cross-claim successor identity
(`:478-504`).
**Durable child-run machinery (the call survives as `call-flow`):**
`child.rs` · parent_run_id, parent_node_id, parent_occurrence,
waiting_child_run_id, waiting_child_occurrence, wait_generation,
invoke_depth, invoke_root_run_id · child queue creation · child
cancellation · release/wake transitions · internal attachment
resolution · actor-mode machinery · flowrunner claimed-run child
delegation + `child:{…}` identities · `InvokeFlowConfig` (replaced by
`CallFlowConfig { flow_id }`).
**Composition-shaped bundle identity:** `ExecutionBundlePackaging` ·
plug manifests · composition-tool identity ·
exact-node/capability-class arms · supplied-component digests
(`catalog/model:924-952,1080-1088`) → the `ExecutionPlanV2` preimage
under the retained field name.
**Recovery-shaped artifact executable model:**
`OccurrenceRecoverySelection` · `ExecutableRecoveryContract` ·
occurrence-recovery bytes/hash · connection-recovery declarations +
compatibility readers (`catalog/model:18,50,441-459`); the artifact
slims to authored graph + public contract.
**Duplicate executable storage:**
`validated_flow_drafts.execution_bundle_bytes` (hash retained as FK) ·
`flow_artifacts.interface_bundle_json/hash` · `component_digests` ·
`occurrence_recovery_json/hash` + their registration parameters and
checks (`catalog-schema:102-107`).
**Outbound effect retry:** the `live-retry` policy arm · repeated-dispatch
records · receiver-behavior assurances · connection-mode retry validation.
**Retention kinds:** `replay-seed` · `audit-seed`;
`release-evidence` added;
`active-attempt` stays (`catalog-schema:1262-1273`).
**Flow authority vocabulary:** `Flow.credentials` · `CredentialRef` ·
`Node::credential` · `Flow.allowed_hosts` + their validation and
contract fields (`flow-model/types.rs:59-70,326-328,503-508`).
**Generated data-API plane:** `crates/data/api` ·
`components/ingress/api-gateway` (CRUD unmounted; the entity-access
kernel survives).
**Embedded scenario engine + stored-suite surface:**
`crates/scenarios/runtime` · `crates/scenarios/catalog` (the
drafts/test-set/report store folds into the management service, B) ·
**`pin-run`** (writes
`test_suites`/`test_cases` — `pin_run.rs:7,23,56`; a future
export-run-as-test-set-file utility is not retained) ·
`event-reader.example`.
**Custom-node plane:** node-host · builder (+ manifests, builder-svc +
toolchain stages) · node-runtime · node-{sdk, guest, invoke, manifest}
(pure standard shapes → flow-model::node_contract; `CapsCtx` + WIT/
HTTP adapters → flowrunner; the custom/recovery descriptor taxonomy
dies in place) · the `wamn:node` WIT · runtime plugins wamn_node +
node_invocation · ctl custom-publish · fixtures/samples:
connection-http-custom, cred-probe, capability-class-×3,
exact-driver-×2, exact-node-×3, disposition-node, node-cred,
sample-node, js-sample, node-ts.
**Composed arm:** flow-driver · node-rs · wac/jco builder legs ·
wave1/wave2 manifests.
**Nodes:** cron · time-shift (registry eleven → nine).
**Ordering/partition plane:** run_queue.partition_key/policy ·
partition_owner · acquire/head/renew/release SQL · janitor
leapfrog/blocking branch · run_dead_letters
(`run-queue.sql:131-139`) · `PartitionPolicy` (run-state + wamn_flow)
· the flow ordering field · registration partition-key extractor
(`registration/model.rs:83`) · materializer ordering mapping
(`decide.rs:190-203`) · admission partition fields
(`admission.rs:61-82`); claim predicate + index collapse to global
FIFO; the dispatcher's mirrored predicate updates in lockstep; lands
in the same change as the reclaim classifier (one claim-path rewrite).
**Rerun/reconstruction:** `reconstruct.rs` · `rerun.rs` · `replay_of`
· `root_run_id` · reconstruction-oriented builders/docs.
**Cron/timers:** the scheduler cron module + `croner`/`chrono` ·
`cron_anchor` + the `runs_cron_anchor` index
(`run-state.sql:286-291,343`) · cron builders/tests · guest
cron/time-shift/delay.
**Stored suites:** `test_suites`/`test_cases` · the `FLOW_TESTS_SQL`
reconciler embed (`run_plane.rs:64-67,1689`) · `authoring_suite_*`
(replaced by the `test_*` set) · copy-env suite blocks (ops).
**Effect-disposition breadth:** requests/outcomes tables
(writer-less, `run-state.sql:87-89`) → `operator_run_actions` + the
terminalize transaction.
**Capture modes:** the four-mode surface + per-flow field → full|off
with the platform byte ceiling; the "always-secretless" claim retired.
**Identity/local:** `authenticate_local` · `identity.local_credentials`
· `identity.sessions` · session/CSRF code.
**Policy switches:** configurable publication policy in the registry.
**Provisioning ops state:** `provisioning.dumps` · copy-saga state →
ops feature schema.
**Impact/backup:** destructive planner branches ·
backup/acknowledge flags · `impact_report` breadth → ops feature.
**Observability extras:** grafana provisioning ·
`provision-dashboards` · trace-relay · the traceproof manifest.
**Demo/POC:** `poc/*` · evaluate-specs · normalize-receipt · hello ·
serve-echo/-node · logspewer · memhog · pgprobe · f1 flow fixtures.

### F.3 Owner resolutions (record)
Single-shot runs + cancel deleted · capture full|off (the literal
secretless guarantee is outside MVP) · effect retry cut — one immutable
attempt and at most one dispatch per effectful occurrence ·
`audit-seed` deleted; no execution consumer found ·
observability named as an outcome, no local Grafana/Loki · fixture
keep-set {sockprobe, connection-http-standard, busyloop} · custom
plane deleted wholesale · call-flow restored and finalized (mandatory
expansion, exact pinning, retained plan bytes, complete
`ExecutionPlanV2`, one effect-uncertain literal, caller-completion
semantics, effectful-node connection requirement) · `check-flow` deleted
(validation via the native plan compiler).
