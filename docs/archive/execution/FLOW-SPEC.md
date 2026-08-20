# Callable Flows — the normative specification

**Status: NORMATIVE DRAFT — REVISION 19 (2026-08-02).**
Self-contained; §20 is history; `callable-flows-spec-rev{1..12}.md.bak` are
archival. Written against `main` @ `a3d8a79`; re-verify pins at each phase
start.

**The project is greenfield — pre-version alpha.** No production, no data to
migrate, no version ceremony. F1–F4 are dev-cluster gate fixtures on an
ephemeral database; `deploy/sql` is the schema of record; from-zero
provisioning is the only "migration" story. Every design decision is a
forward decision. External reviews default to brownfield assumptions — each
finding is tested against "we can delete everything and reprovision," and
against the vertical slice: **nothing gates the first callable flow unless
the first callable flow needs it.**

**Owner decisions in force:** the event input contract changes freely (its
golden test documents the *current* shape — no freeze language); `fail` is
universal hard failure; disconnect cancellation best-effort with the honest
orphan bound (§10.6);
durability uniform, cost measured (§17); sagas a capability requirement on
primitives (§12.7); exposure versions inside the catalog release; catalog and
run plane share one PostgreSQL database per project environment; single-hash
activation with the recorded upgrade path (§5.4); **the coarse promotion
rule** — a release cannot replace the applied release while any nonterminal
run is pinned to it (§5.4a); deferred seizure (§10.7); **transparent retry
recovery scoped to an unchanged `(attachment_id, definition_hash)`** (§6.2);
**caches pay their own way** — services read the authoritative views until a
cache demonstrably earns generation machinery (§7.4); event-registration
exposure is a deferred tranche (§5.7); **run context** — every completion
emission is `{output, ctx?}`: every node reads context, any node may write
it, a write **replaces** the document (merge is an expression via `merge()`,
never platform semantics) (§4.4); the invocation handshake is **two-stage**
(`begin → wait | cancel`) (§11); child revocation gates **creation**, never
recovery of an existing child (§12.2).

**The standing rule:** any mechanism whose acceptance test cannot be written
at its own implementation phase is flagged for deferral review.

**Deferred constants** marked `DEFERRED(owner)`: idempotency TTL and outcome
retention, JSON-Schema resource limits, perf budgets, purge retention
windows, sweep cadence.

---

## 0. The model

| Concept | Answers | Lives in | Identity |
|---|---|---|---|
| **Entry node** | how the flow starts; its input contract | flow graph | flow artifact |
| **Response node** | what the caller receives; when released | flow graph | flow artifact |
| **Flow artifact** | immutable graph + pinned executable contracts, including `pure | effectful` policy | `flow_artifacts` | `(tenant, flow_id, flow_version)` — flow IDs are tenant-scoped; `catalog_id` scopes *release* identity |
| **Source / attachment** | which schedule / credential policy / caller policy drives which flow | catalog release | release |
| **Activation** | is this attachment's confirmed definition live here, now | operational overlay | confirmed hash |
| **Release** | the complete, validated project definition | `CatalogRelease` | `(catalog_id, catalog_version)` |

**R1** — Every flow has exactly one entry node; its type is contract surface.
**R2** — In a `request`-entry flow, every structurally successful branch
before release reaches exactly one author response node; platform failures
and cancellation may release without one.
**R3** — `respond` releases the caller without ending the run; `fail` ends
the run, releasing an error if a caller is still attached.
**R4** — The flow artifact carries no invocation-source configuration.
**R5** — Exactly one caller outcome (caller CAS) and exactly one run terminal
(terminal CAS), arbitrated separately, each returning a typed result (§9.6).
**R6** — Every run is durable; placement inline or queued. R6 remains the
sole execution model if the §17 budgets pass.
**R7** — Every executor-owned write is fenced by its lease generation (§9.5);
effect authority is held by deferred seizure (§10.7); capability-context
fencing stages with the H9 runtime work.
**R8** — Cross-run and reconciler transitions name the authority they seize:
wait tokens for parent wakes; generation seizure for child cancellation and
the deadline sweep.

Publication pins each implementation's `pure | effectful` policy in the same
resolved-node contract used by standard and custom nodes. Pure occurrences write
no effect-ledger row. An effectful occurrence records one immutable write-ahead
attempt, at most one dispatch, and an outcome when known. HTTP method, mutable
node configuration, capture mode, and endpoint behavior cannot authorize another
dispatch.

---

## 1. Entry nodes

### 1.1 The set

| Type | Starts on | Caller | Input |
|---|---|---|---|
| `request` | invocation (HTTP, internal, studio) | **yes** | author-declared (§4.1) |
| `event` | a durable row event | no | §4.3 |

`message` and `blob` are expected extensions. `manual` is not an entry type:
the studio test-run is a `request` entry behind a `studio` attachment.

### 1.2 Exactly one

`count(nodes where type ∈ ENTRY_TYPES) == 1` (`no-entry-node`,
`multiple-entry-nodes`). One input contract, one response rule, one compiled
signature per artifact. Scheduled flow entry is unavailable in the MVP and
cannot be encoded as a wrapper node. `Flow::entry` does not exist; the entry is
the unique entry node.

### 1.3 No source configuration in the artifact

```json
{ "id": "in",   "type": "request",
  "config": { "input-schema": { "type": "object", "required": ["po-number"] } } }
{ "id": "on-disposition", "type": "event" }
```

### 1.4 Engine-reserved semantics

Entry nodes never enter ordinary dispatch: the engine synthetically records a
completed entry step carrying the admitted payload and enqueues its `main`
successors. (The engine currently seeds the entry as an ordinary `Token`;
that changes.) Likewise `respond`, `fail`, and `invoke-flow`.

---

## 2. Response and failure nodes

### 2.1 `respond` — release with success

Pure pass-through body. Payload delivered through the caller CAS and recorded
as the caller outcome, distinct from `result_json`; releases the caller;
execution continues along its zero-or-one `main` edge — **zero → caller
release + terminal `completed` + queue completion in one transaction** (§9.8).
Request entries only; always semantic success. Status from `config.status`,
range **`200..=599`** — informational 1xx responses are non-final and cannot
map onto a one-result invocation interface; the range is deliberately not
narrowed to success codes (an author may surface an upstream failure
explicitly). No-body statuses (e.g. `204`) are an ingress-implementation
concern, settled there.

### 2.2 `fail` — universal hard failure

```json
{ "id": "nope", "type": "fail",
  "config": { "code": "po-not-found", "message": "PO not found", "status": 404 } }
```

Authored contract (restored — it was present in rev 4 and lost in the rev-10
flattening): `code` required; `message` optional; `status` optional,
**default `400`, range `400..=599`**. Ends the run `failed`; no outgoing
edges (`fail-has-outgoing-edge`); iff a caller is attached, releases the §8
envelope through the caller CAS with `caller_outcome_kind = failed` and
`caller_http_status = config.status` — so an authored business failure has a
normative HTTP mapping, not just an envelope. Discards live frontier tokens;
permitted for every entry type. `respond` is caller-scoped, hence
request-only; `fail` is run-scoped, hence universal.

### 2.3 Ports

`main`, node-declared ports, and the **reserved** `error` port — engine-
routed on node failure, never emitted by choice (verified: with no `error`
edge, `error_or_fail` fails the run). All non-`error` ports are completion
ports.

---

## 3. Structural validation

### 3.1 Inputs

`validate(flow, resolved_interfaces)` — custom output ports live in
`NodeManifest.output_ports`. The resolved interface bundle is pinned at
publish and participates in artifact identity. At runtime, an emitted port
outside the pinned set is `infrastructure-failure`.

### 3.2 Regions

Stoppers = `respond` | `fail` nodes. **S** = nodes reachable from the entry
without traversing through a stopper (stoppers reached are members).
**R** = stoppers in S. **Resp** = `respond` nodes in S. **Caller-gone** =
nodes reachable from any `respond` node's successors.

### 3.3 Predicates — `request` entries

| # | Predicate | Error |
|---|---|---|
| P1 | every completion port of every non-release node in S has exactly one outgoing edge | `unanswered-port` / `same-port-fanout-before-release` |
| P3 | every node in S reaches some node in R | `unanswerable-path` |
| P4 | no `respond` reachable from any release node — full graph | `double-release` |
| P5 | caller-gone ∩ S = ∅ | `region-re-entry` |
| P6 | **Resp** non-empty | `no-response-node` |

P1 is port-level; P3 catches trapped components; P4 spans the full graph; P5
uses the full caller-gone set. **P6 requires a reachable `respond`, not
merely a stopper** — a request flow whose every path ends in `fail` can never
succeed and is an authoring error; the error name and the set now agree
(previously P6 tested R, so an all-`fail` flow passed a check named
`no-response-node`). Runtime dispatch budgets remain necessary.

### 3.4 `event` entries

No `respond` node (`respond-without-request-entry`); `fail` permitted; **the
request predicates P1/P3–P6 are inapplicable** — S is still well-defined by
reachability (it is not "empty"); the predicates simply do not constrain
non-request entries.

### 3.5 General integrity — all entries

Exactly one entry; entry has no incoming edges; every node reachable — **an
error** (the existing warning-level test flips); at most one `error` edge per
node (`multiple-error-edges`); `fail` edge-free; no `delay` in S — **request
entries only** (`delay-before-release`): the rationale is a caller waiting,
so with S now correctly nonempty for event flows the rule must not bind them.

---

## 4. Input contracts

Entry output is the business payload; invocation metadata rides the trusted
context (§12.6).

### 4.1 `request`

`config.input-schema`: JSON Schema draft 2020-12, no remote `$ref`, validated
before run creation (400, no run row). Limits `DEFERRED(impl)`.

### 4.2 Scheduled entry — unavailable

The MVP has no scheduled flow-entry contract. A document containing a `cron`
entry node is refused as retired vocabulary.

### 4.3 `event` — normative

`{ "event": …, "new": …, "old": … }`, absent images omitted. Replaces the
current run input (`{trigger, entity?, table, event, seq, payload, old?,
causation}`): `trigger`/`entity`/`table`/`seq` move to the invocation
context; `causation` becomes lineage columns; `payload`'s
operation-dependent meaning is removed. The golden test in
`materializer/src/input.rs` documents the **current** shape and is updated in
the same commit as the shape — there is no freeze language to maintain, only
a test that says what the shape is now. Fixtures and stored scenarios are
hand-edited (§16).

### 4.4 Run context — durable per-run memory (owner decision)

The engine's data model is single-input: a node receives its predecessor's
output, which is its entire world. That invariant stays. **Run context** is
the per-run document that carries state *across* nodes without breaking it —
and it rides the emission itself:

**Every completion emission is `{output, ctx?}`.** `output` becomes the
successor's payload, exactly as today. `ctx`, when present, **replaces the
run context document** — atomically with the emission's processing, so
context and payload advance together or not at all. There are no platform
merge semantics: `merge()` is a JMESPath builtin, so a developer who wants
merge writes `merge(context(), {hold: rows[0]})`, and deep-merge's edge-case
zoo (arrays, nulls, nesting) is unrepresentable rather than specified.
Error-port emissions carry no `ctx` — failure paths do not mutate durable
memory.

**Writing, per node kind:** standard nodes take an optional per-instance
config key `"ctx"` — a JMESPath expression over the node's **output** (with
`context()` available), evaluated by the SDK; absent means no write. Custom
nodes return `ctx` natively in the invocation envelope (a wire-contract
delta at the runner→node hop). No dedicated writer node exists: a
`transform` with expression `"@"` and a `ctx` key is a pure
passthrough-setter, so a separate node type would be redundant.

**Reads are universal.** Every node is passed the context alongside its
input: `RunContext` gains `context: &Value`, the custom-node envelope gains
a `context` field, and the expression surface gains one custom JMESPath
function, **`context()`**, usable in any node's params and expressions
(`"params": ["context().hold.id", "rows[0].qty"]`). Existing expressions are
untouched; no input key can collide; both reads (`context()` in
expressions) and writes (`ctx` keys in config) are **statically detectable**
by scanning the graph JSON — ambient at runtime, explicit in the document.

**Execution context is single-shot.** The context lives in the in-memory
`ExecutionState` for one graph walk and is size-capped at the owning boundary.
There is no durable context reader or engine restore contract. Context-resolved
parameters for an admitted effectful attempt are part of its immutable recorded
input (`attempt_input_ref`, §10.3), so the audit fact states what the attempt
actually saw. Capture mode neither permits nor prevents a dispatch.

**Scope rules:** per-run; born empty; child runs from `invoke-flow` start
with fresh context (the invocation payload is the only inheritance); writes
apply in dispatch order — sequential today; a deterministic ordering rule is
a named prerequisite of any future parallel dispatch.

---

## 5. The catalog release

### 5.1 Current state (verified)

**Superseded — retained as the before-picture 5.2 exists to replace.** The flow
half is realized: `wamn-0h0g.12.102` (`e45ca35b`) retired `wamn_run.flows` and
`deploy/sql/flows.sql`, so the mutable registry described below no longer
exists and `catalog.release_flows` (5.2) is what ships.

Version-scoped: `entities`, `fields`, `relations`, `indexes`, `constraints`.
Unversioned: `rls_policies`, `seed_datasets`, `event_registrations`. Flows
live outside the catalog (`wamn_run.flows`, mutable `active`) and **flow
versions are mutable** (`register_flow` "refreshes its graph";
`copy_project_env` upserts). `publish-catalog` is a sequence of operations.
Applied-ness is a partial unique index over version rows — no stable head
row.

### 5.2 Immutable flow artifacts

```
flow_artifacts
  tenant_id, flow_id, flow_version
  schema_version, graph_json, graph_hash
  interface_bundle_hash, component_digests, created_at
```

Immutable after insert, **enforced in the database** (UPDATE-rejecting
trigger or revoked privilege). Identical re-insert = no-op; differing content
= `flow-version-content-conflict`. `register_flow` and copy-env are rewritten
against artifacts — code replacement, not data migration.

`catalog.release_flows (tenant_id, catalog_id, catalog_version, flow_id,
flow_version)` FK → artifacts; one version per flow per release.

Interface pin ≠ implementation pin: the bundle pins ports; component digests
pin client-supplied executables; standard-node behavior is pinned only by
`runs.platform_revision`; executing under a different platform revision is not
behavior-identical.

**Release membership is phased (§19):** the 2A spine carries
`release_flows` + `http`/`internal` attachments + their sources; RLS and seed
re-versioning join in 2B; event registrations in their
own tranche (§5.7).

### 5.3 Release application — preflight, then one transaction

**`catalog_heads`: one row per (tenant, catalog_id, environment)** — created
at first application, holding the applied version; the lock object for both
publication and admission ("the applied version row" is not stable and does
not exist at first application).

**Preflight (no transaction held):** resolve inputs, validate flows, compile
routes/schemas, build the invocation graph, plan DDL/RLS, compute hashes,
reject non-transactional DDL, record the expected base.

**The release transaction:**

```
lock catalog_heads FOR UPDATE; reject stale base
verify input hashes and tombstones
enforce §5.4a: zero nonterminal runs pinned to the applied release
  (admissions hold the same row FOR KEY SHARE — none can appear after this)
project-schema DDL
persist immutable release members
carry-forward activation rows
migration journal row
update catalog_heads.applied_version
```

Full rollback on any failure. Seeds are definitions; materialization is a
separate audited operation.

### 5.4 Activation — single-hash

```
attachment_activation           -- current state, one row per attachment
  tenant, catalog_id, environment, attachment_id
  confirmed_definition_hash, enabled, changed_at

attachment_activation_events    -- append-only transition audit
  …, event_seq, enabled, confirmed_definition_hash,
  changed_at, changed_by, reason
```

Runtime authorization = enabled AND pinned hash = confirmed hash. Confirming
a new hash disables older generations by construction — a parked run pinned
to H7 resuming after H8 is confirmed receives `callee-revoked` **on its next
child creation**: revocation gates *creation* of new children, never
recovery of an already-created child, which proceeds under the pinning taken
at its creation (§12.2). Explicit, never silent. Disable is the emergency lever. The
definition hash covers the complete effective contract **including resolved
sources** (an auth or caller-policy change under an unchanged source ID
changes every referencing attachment's hash). Carry-forward: same ID + same
hash → carries; changed hash → disabled pending confirmation, listed in the
promotion report; new ID → disabled; removed ID → inactive and **tombstoned**
(no reuse — what makes stable IDs safe as idempotency namespaces). Upgrade
path recorded: generation-keyed activation + a revocation overlay, when a
real drain scenario writes its acceptance test.

### 5.4a Promotion — the coarse rule

> **A release cannot replace the applied release while any nonterminal run is
> pinned to it.** The operator waits for the runs to finish, or cancels them
> (fenced, via the sweep's seize shape), and retries publication.

Enforced inside the release transaction under the head lock (§5.3), so no
old-release run can be admitted after the check. This is strictly safer than
any impact classification — it blocks everything classification would have
analyzed — and it is trivially explainable and testable.

**Deleted by this rule** (each returns only when operational evidence writes
its acceptance test, per the standing rule): dependency extraction and the
unknown-dependency model; schema-compatible/incompatible classification;
drain / expand-contract planning; the attachment-revoking class and its
audited override; the stable affected-run set. The pain that will eventually
justify their return — a long-parked run blocking all promotion — is exactly
the operational evidence to design against, and does not exist in a
fixture-only alpha.

### 5.5 Lifecycle

Remove-from-release / disable / retire in the alpha; **physical purge and
retention workflows are 2B** (`DEFERRED(owner)` windows).

### 5.6 Contract surface

Entry type, input schema, and (later) output/error schemas are the declared
contract; compatibility = canonical equality or an explicit contract-major
bump.

### 5.7 Event exposure — deferred tranche

**Now:** event flows in full — entry type, §4.3 shape, materializer rewrite,
context carriage. Publishing an `event`-entry flow validates a registration
exists in the live table. Event runs record `catalog_version`,
`registration_id`, and the registration **content hash** (full row under a
hash-schema version): historical trigger-definition reconstruction is unavailable while the
registration is mutable, but tampering is detectable — stated, not hidden.
**Deferred with its acceptance tests:** registration release-membership and
version key; RI-ordering in the release transaction; the JetStream
durable-consumer reconciler; event activation.

---

## 6. Admission

### 6.1 Three stages, every producer

**A** — candidate resolution (reads only) — **producer-shaped**: HTTP
resolves a route against the applied release + activation overlay
(**including disabled definitions, for recovery lookup**, §6.2); events
resolve the **live registration** (§5.7) —
there is no route. **B** — bounded external work (own timeout, no writes) —
**HTTP-specific in its full form**: authenticate under the route-selected
policy, read bounded body, map (§7.3), validate, fingerprint. Cron's B is
tick synthesis (§4.2); the materializer's B is event-input synthesis
(§4.3). **C** — final admission, one transaction, **through the single
`admit` transition in the `run-state` transitions module**: `FOR KEY SHARE`
on `catalog_heads`, verify expected version, **perform the
producer-specific definition check** (the §6.1 table: attachment activation
for HTTP/internal; the current registration content hash, recorded, for
events — which have no attachment to check), idempotency check-and-insert,
run row + queue row **in its
producer-determined initial state** (the §6.1 table: claimed for inline
HTTP; available for event). Changed between A
and C → retry once, then `admission-retry`.

**All run producers use the same transition and the same lock** — but each
carries its **own admission identity**; the ledger's HTTP shape is not forced
onto producers that have no principal or client key:

| Producer | Dedup identity (enforced by) | Definition check | Initial queue state |
|---|---|---|---|
| HTTP invocation service | `(attachment, principal, client key, fingerprint)` — `invocation_admissions` (§6.2) | attachment activation (§5.4) | **claimed** to the invocation service's executor identity (inline execution) |
| event materializer | `(tenant, registration_id, event seq)` — the existing `runs.idempotency_key` unique partial index, key shape `evt:{registration}:{seq}`: the **event dedup scope**, exactly one run per registration per event sequence | **live registration content hash, recorded** (event activation is deferred, §5.7 — there is no attachment to check) | **available** — unclaimed |

The producer variant therefore determines **identity, definition check, and
initial queue lifecycle** — not identity alone; "always claimed to the
invocation service" was an HTTP-ism that would strand every queued run.
`admit()` commonizes the head lock (`FOR KEY SHARE` on `catalog_heads`) and
the run+queue insert. None may bypass it. `flow-http` preflights only.

**The queue row is always created at admission, in its producer-determined
initial state** — claimed to the invocation service's executor identity for
inline HTTP; available (unclaimed) for an event, where a worker claims it.
A different executor taking a claimed run is an ordinary queue handoff.

### 6.2 The admissions ledger

```
invocation_admissions
  tenant_id, catalog_id, environment, attachment_id
  definition_hash                  -- the scope guard
  principal_digest, client_key_digest
  client_request_fingerprint
  admitted_catalog_version, admitted_flow_version   -- recorded facts
  run_id, created_at, expires_at
UNIQUE (tenant, catalog_id, environment, attachment_id,
        principal_digest, client_key_digest)
```

**`client_request_fingerprint`**: method, normalized authority/path/query
(canonical percent-encoding; repeated values in wire order; empty distinct
from absent), content type, canonical body digest. Headers do not
participate: recovery is scoped to an unchanged definition (below), so
mapping-aware header participation is unnecessary; two requests differing
only in a mapped header under one client key are a client misuse of that key,
and are documented as such.

**The recovery scope (owner decision):**

> Transparent retry recovery is guaranteed only while the same
> `(attachment_id, definition_hash)` remains current. Any material
> definition change ends it with a clear `idempotency-scope-changed`
> conflict — never a second run.

An *unchanged* attachment across a promotion keeps its hash, so the dominant
case — post-release retry across a promotion — still returns the stored
outcome. (This deletes the preimage-spec machinery and the separate
`auth_source_hash`: the definition hash already covers the resolved auth
source, so an auth change ends the scope through the same comparison.)

**Lookup order (normative — corrects a real contradiction: the route selects
the attachment, and the attachment supplies the auth policy, so
authentication cannot precede resolution):**

```
1. match the route against release definitions,
   including DISABLED definitions (recovery lookup only)
2. obtain the attachment ID, definition hash, and current auth policy
3. authenticate under that policy
4. look up the admissions ledger
5. matching released admission, hash current → stored outcome,
   even if the attachment is currently disabled
   matching admission, hash changed → idempotency-scope-changed
6. no matching admission → require current activation; map, validate, admit
```

A removed route ends transparent recovery (stated); a **disabled** route does
not — disablement blocks *new* admissions, never outcome retrieval.

Cases: in-flight duplicate → 409 (no join); released → stored outcome;
different client fingerprint → 409 `idempotency-key-reused`; changed
definition → `idempotency-scope-changed`; required key absent → reject
pre-run; retention elapsed → `outcome-expired`. TTL `DEFERRED(owner)`;
invariant: an admission may not expire while its run is active, and the
outcome remains retrievable for the promised window.

---

## 7. Sources, attachments, exposure

### 7.1 Definitions

No `enabled` in definitions; kinds `http | internal | studio`;
entry-kind matching (`attachment-entry-kind-mismatch`); route uniqueness
`(host, path, method)` over normalized templates within the release (one row
per method, wildcard-host sentinel); `deadline-inversion`.
Activation-transition rules (serialized per environment): one enabled
`internal` per flow; no invocation cycle among
enabled attachments of the active release; route unambiguity; env policy.
**Auth sources define policy, never material** — rotation never requires a
release; the context records the key-generation ID only.

### 7.2 Route grammar

Lowercase host; explicit non-default port only; `*` sentinel; exact host
beats wildcard. Segments static | `{param}` | one trailing `{*catch-all}`;
precedence static > param > catch-all; parameter names do not distinguish
templates. Percent-decode per segment; `%2F` never splits; trailing slash
normalized; paths case-sensitive. Trusted-proxy handling is deployment
configuration.

### 7.3 Input mapping

JSON Pointer destinations (`""` = root; the body may establish the root;
static parent/child overlap checks apply among the transport mappings;
collisions with mapped-body fields are runtime 400s). Merge order body →
path → query → headers; duplicate destinations are publish errors. Sources
required unless `optional`. `cardinality: one | many`; repeated values into
`one` → 400; `many` yields an array in wire order. No coercion. Header names
lowercased. Mapped-size ceiling. Protected headers rejected at publication:
`authorization`, `proxy-authorization`, `cookie`, `set-cookie`, `x-wamn-*`,
deployment identity headers.

### 7.4 Projections — views first; caches pay their own way

The route table and activation views are ordinary SQL over
applied-release ⋈ activation, joined on exact definition-hash equality.
**Services read the authoritative views directly.** Any service that later
introduces a cache must add and validate a generation-based invalidation
protocol in the same change — generation machinery does not exist until a
cache does. `catalog_heads` remains (it is the lock, not a cache);
`catalog_heads.applied_version` is the observable release edge if one is ever
needed.

### 7.5 Cron

Scheduled attachments, anchors, catch-up, and firing identity are outside the
MVP. They do not supply a hidden third flow entry type.

---

## 8. Envelopes and rejection

Every caller outcome has a durable body: `responded` = the author payload;
`failed`/`cancelled` = `{error: {code, message?, run-id, flow-id,
flow-version}}` — `message` **optional**, matching `fail.message` (absent
means the `code` speaks for itself).

**`caller_http_status` is defined for every failure kind, not only authored
`fail`** (finding-of-record: the interface required a stored status that
only one path produced):

| Kind | Stored status |
|---|---|
| authored `fail` | `config.status` (default 400) |
| unhandled node error / `infrastructure-failure` / `effect-uncertain` | 500 |
| response-deadline exceeded | 504 |
| cancellation — observed disconnect | 499 (stored for idempotent retrieval; the disconnected client sees nothing) |
| cancellation — operator / run-deadline (pre-release) | 499 with the distinguishing `code`; `cancel_kind` carries the cause |

Defaults, not policy: a fronting layer may remap; the stored value is what
`wait` returns. Pre-run rejections carry **no `run-id`**: 404 / 401 / 403 /
413 / 400 / 409 / `admission-retry` / `idempotency-scope-changed` /
`outcome-expired`.

---

## 9. Caller outcome, run terminal, write authority

### 9.1 The caller CAS — queue-joined, fenced

The generation lives on **`run_queue`** (`runs` owns lifecycle; the queue
owns authority):

```sql
UPDATE wamn_run.runs AS r SET
    caller_outcome_kind = $1, caller_outcome_json = $2,
    caller_http_status = $3, caller_release_node_id = $4,
    caller_outcome_hash = $5, caller_released_at = now()
FROM wamn_run.run_queue AS q
WHERE q.tenant_id = r.tenant_id AND q.run_id = r.run_id
  AND q.lease_owner = $owner AND q.lease_generation = $gen
  AND r.tenant_id = $t AND r.run_id = $run
  AND r.caller_released_at IS NULL
  AND r.status IN ('dispatched','running')
```

All five releasing paths compete here. **These joins are written once**: a
transitions module owned by `run-state` exposes `admit`, `release_caller`,
`terminalize`, `wake_parent`, `park`, `complete`, and nothing else writes
these rows. **Enforcement strength, stated honestly:** in the alpha this is
static conformance — SR27/SR28-style source, dependency, and SQL-ownership
checks — not a database guarantee; database roles or stored procedures are
the later hardening *if required*, never adopted merely to make wording
literally true.

### 9.2 Repeated caller-release equality

A repeated `respond` submission is benign iff kind, canonical body hash, HTTP
status, and release node id all match; any mismatch is
`infrastructure-failure`. This is idempotent CAS comparison, not graph replay.

### 9.3 The terminal CAS — first durable terminal wins

Same queue-joined fenced form, guard `status IN ('dispatched','running')`.
Pre-release paths take caller CAS + terminal CAS in one transaction;
`respond` takes caller CAS + current-turn state + queue + lease in one
transaction (+ terminal when no successor); post-release terminal paths take
the terminal CAS only — a losing post-release `fail` touches no caller state
**and still fails the run**. Sweep and cross-run cancellations reach it after
seizing the generation.

| Race | Caller | Run |
|---|---|---|
| `respond` first | responded | continue (or complete, §9.8) |
| pre-release `fail` / error | failed envelope | `failed`; discard; release lease |
| pre-release deadline / cancel / disconnect | cancelled envelope | `cancelled`; discard; release |
| post-release `fail` | no-op | `failed` |
| post-release run-deadline / operator | no-op | `cancelled` |
| disconnect after release | no-op | no effect |

### 9.4 Waiting and disconnect

Waiter = NATS subject `wamn.run.{tenant}.{run_id}.response` + bounded poll of
`caller_released_at`. One caller, one wait: 409 in flight; stored outcome
after. **Observed** disconnect before release → durable cancellation; an
unobserved one (ingress death) runs to the response deadline — the orphan
bound, enforced by §10.6.

### 9.5 Lease fencing (R7)

`run_queue.lease_generation`, incremented on every claim/reclaim; every
executor-owned mutation goes through the queue-joined form; the generation is
never duplicated onto `runs`. Control-plane bumps are subject to deferred
seizure (§10.7).

### 9.6 Typed transition results

```rust
enum CallerReleaseResult {
    Released,
    AlreadyReleased(StoredCallerOutcome),  // exact duplicate comparison permitted
    RunTerminal(RunTerminalState),
    FenceLost,                              // STOP: no reads, no continuation
    NotFound,
}
```

Distinguished by a follow-up read in the same transaction; the terminal CAS
returns the analogue. `FenceLost` is absolute.

### 9.7 Cross-run authority (R8)

The parent wake requires the **parent's** state, in one transaction with the
child's release: nonterminal + `waiting_child_run_id` +
`waiting_child_occurrence` + `wait_generation`. A parent cancelling a child
seizes the **child's** generation first.

### 9.8 Zero-successor `respond`

Caller release + terminal `completed` + queue-row completion in
one commit — the pure request path is **two commits**; §17 measures both
shapes.

### 9.9 Natural completion — frontier exhaustion

The rule finding-of-record: zero-successor `respond` was the only defined
successful terminal, while event flows and post-release continuations
end by draining. Normative rule:

> When an executor turn ends with an **empty frontier**, **no failure**, and
> **no caller attached or the caller already released**, the executor takes
> the `terminalize(completed)` transition (fenced, queue-joined) in the same
> transaction as queue-row completion.

This is how an `event` run and a post-release continuation reach
`completed` — including through an intentionally unwired completion port,
whose token is simply dropped from the frontier. A request-entry run with an
empty frontier and an **unreleased** caller is unreachable by construction
(P3/P6 guarantee a release node on every successful path; platform failures
release through their own transitions).

---

## 10. Execution and durability

### 10.1 Placement

Inline (`request` runs execute in the invoking service's turn) or queued
(`event` arrivals; any run that parks). No transient mode.

### 10.2 Single-shot execution state

The graph frontier, retry cursor, visit counts, and authored context exist only
in the current in-memory engine state. Neither `state_json` nor completed
`node_runs` is an engine snapshot, and no transition reconstructs a frontier.
After claim loss, the claimant may restart from zero only when no immutable
effect attempt exists. Any immutable attempt instead yields the fenced
`effect-uncertain` classification described in §10.3.

### 10.3 Node-attempt protocol

Publication resolves every standard or custom implementation to one
`ResolvedNodeContract` whose effect policy is exactly `pure | effectful`.
Neither a node type/configuration table nor an environment assertion may replace
that policy.

For each effectful occurrence: (1) persist one immutable write-ahead attempt with
the trusted root-run, frame, local-node, occurrence, current-plan, source-artifact,
connection, and credential facts; (2) acquire the wire-I/O permit by the first
successful dispatch insert for that exact occurrence; (3) send; (4) append the
terminal outcome when known. Pure occurrences skip all four protocol operations.

Attempt and outcome retries are exact-idempotent: the same complete immutable
facts return the existing row and any difference refuses. A sent attempt without
a recorded outcome is **`effect-uncertain`** and never sends again. More
conservatively, reclaim seeing any abandoned effectful attempt marks the run
uncertain without invoking the flowrunner. HTTP verbs do not change this rule.
Capture is independently optional and has no role in effect authority. Attempt,
dispatch, and outcome rows are capture-exempt immutable protocol facts.
`node_runs.status` is `started | parked | success | error`.

### 10.4 Inline lease ownership

The queue row exists from admission in its producer-determined initial
state (§6.1); once claimed it is held by a real executor identity +
generation; claim authority is lease-based; the sweep-only variant is a measured
optimization.

### 10.5 Deadline/cancellation runtime work

Nothing enforces deadlines today (`set_epoch_deadline(u64::MAX / 2)`, "No
kill semantics"). Required: absolute deadlines persisted at admission;
per-invocation epoch deadlines; host-side call timeout; deadlines on
outgoing HTTP/DB; credential revocation on cancel; instance disposal; fenced
lease release; child propagation; `cancel_kind`/`terminal_reason`. Shares the
runtime surface with the open **H9** findings and is scheduled with them —
**but H9 closes on `node-host`'s own findings, nothing else**; `flow-http`'s
adversarial suite is prevention for a new component, not H9 remediation.

### 10.6 The deadline sweep — dispatcher-owned, seizing

The dispatcher (the deployable; `wamn-scheduler` is a pure crate) sweeps
partial indexes on the two deadline predicates: lock (`FOR UPDATE SKIP
LOCKED`, bounded batches) → recheck → **DEFER if the current attempt is
`started` within `attempt_deadline_at`** → seize (increment generation) →
terminal CAS (+ caller CAS if unreleased) → propagate, clean, commit, notify.
Honest orphan bound = response deadline + max sweep lag + max attempt
deadline.

### 10.7 Effect authority — deferred seizure

No authority may seize the generation of a run whose current attempt is
`started` and within its `attempt_deadline_at`; seizure waits out the
attempt. An executor inside its attempt window *is* the authority; resuming
past it is refused by the pre-dispatch check (reject if `attempt_deadline_at`
or the invocation budget has passed, at the host boundary, immediately before
effect dispatch). **A deferred cancellation is never forgotten:**
`runs.cancel_requested_kind/at` persists the request; the sweep re-visits
after the attempt deadline; the attempt-completion transition checks it and
terminalizes. Costs stated: cancellation latency inside a live attempt is
bounded by the max host-call deadline. Capability-context fencing stages with
H9; when it lands, seizure becomes immediate and this rule retires. Passing the
pre-dispatch checks is necessary but not sufficient: the first successful
dispatch insert for the occurrence is the sole wire-I/O permit.

---

## 11. Ingress

`components/ingress/flow-http`: thin adapter — resolution (including
disabled-definition recovery lookup, §6.2), normalization, auth, mapping,
limits, idempotency preflight, waiting, observed-disconnect cancellation,
HTTP adaptation — over the versioned invocation interface; **not a `runs`
writer**. Replaces `poc/webhook-f1`. WAC composition is a transport
optimization. **The interface — a two-stage handshake** (finding-of-record:
the single-shot form returned a run-id only with the terminal result, making
`cancel(run-id)` unusable exactly when disconnects are observed — while
waiting — and gave stage C no way to detect drift from the adapter's
preflight):

```wit
record invoke-request {
  attachment-id: string,            // resolved by the adapter (route match, §6.2)
  expected-catalog-version: u64,    // what preflight resolved against —
  expected-definition-hash: string, //   stage C detects A↔C drift with these
  client-request-fingerprint: string, // computed in stage B; the service
                                      //   cannot recompute it without the raw request
  payload: json,                    // mapped, validated business payload
  idempotency-key: option<string>,
  principal: principal-ref,         // authenticated identity, adapter-verified
  deadline-override: option<u64>,
  trace: option<trace-context>,
}

begin: func(req: invoke-request) -> begin-result
variant begin-result {
  admitted(admitted),               // admitted = {run-id} — the durable handle,
                                    //   held while waiting; this is what makes
                                    //   observed-disconnect cancellation real
  rejected(rejection),              // PRE-RUN: no run exists
}
record rejection { status: u16, code: string }   // 400/401/403/404/409/413,
                                                 // admission-retry,
                                                 // idempotency-scope-changed,
                                                 // outcome-expired

wait:   func(run-id: string, timeout-ms: u32) -> option<invoke-result>
                                    // BOUNDED: none = still running; the
                                    // adapter alternates bounded waits with
                                    // connection-liveness checks — a blocking
                                    // wait could never observe the disconnect
                                    // it is supposed to react to
variant invoke-result {
  responded(response),              // response = {run-id, body, status-hint: option<u16>}
  failed(failure),                  // failure = {status: u16, error: flow-error}
  cancelled(failure),               //   — carries the STORED caller_http_status
                                    //   (§2.2 fail persists it; without this,
                                    //   an authored fail 400 could not reach
                                    //   the wire as a 400)
}

cancel: func(run-id: string) -> cancel-ack       // observed-disconnect path
```

`rejected` is how final-admission races and pre-run refusals surface without
inventing a run. The adapter holds the run-id from `begin` and loops bounded
`wait` calls, checking its client connection between them; an observed
disconnect calls `cancel(run-id)`. No `accepted` variant in
`invoke-result` — the platform never invents an acknowledgment. Hardening from day one: bounded supervised concurrency;
body limits enforced during read; cheap checks before the bounded read,
signature verification after; read/idle/whole-request timeouts. Lands with
its `package-roles.json` entry.

---

## 12. Internal invocation and sagas

**12.1** `invoke-flow`; legal in both regions; not egress. **12.2**
Resolution: `parent.catalog_id + parent.catalog_version + flow-id`, current
activation under the single-hash rule → `callee-revoked` when unconfirmed or
disabled — **checked at child creation only**: recovery of an
already-created child (occurrence-keyed, 12.3) never re-authorizes; the
existing child proceeds under the pinning taken at its creation. The child
records the full identity tuple. **12.3** Durable
protocol: create-or-recover keyed by the parent occurrence
(`Dispatch::occurrence`, `run:node:occurrence`, §15.1 uniqueness); wake at
child **release** in one transaction under §9.7's dual authority;
post-release child failure never touches the parent; a reconciler
re-delivers lost wakes. **12.4** `main` = response payload; `error` =
envelope (`callee-cancelled` / `callee-revoked` / `callee-timeout` / child
`fail` / unhandled). Child flow version pinned at invoke. **12.5** Child
response deadline = min(config, parent's remaining region budget, child
limits); child run deadline its own; cancellation propagates **only before
child release**, by seizing the child's generation; depth cap (default
8/env) + per-root fanout caps. **12.6** Caller policy: `allowed-callers` +
actor mode `inherit` / `service` / `attenuate` — **`service`, normatively:
the callee executes under its own service identity (derived from environment
+ flow), and the caller's identity and lineage are retained in the child's
trusted context for audit**; runtime authorization always;
the trusted context is persisted (versioned, size-capped, capture-exempt,
never author-controlled). **12.7 Sagas**: no construct; the primitives
(exactly-once child creation, wake-at-release, envelope error edges,
pre-release cancellation, lineage, one-dispatch effect facts, run status) must
suffice for author-built compensation — Appendix B is the acceptance,
scheduled **after** the HTTP vertical slice (§19); failing it fixes
primitives, not adds a DSL.

---

## 13. Testing

No debug/injection node. The harness seeds runs; `ScenarioClock` + doubles
supply entry non-determinism; §4.2/§4.3 are the synthesis targets.
Partial-flow testing = constrained durable-state construction,
scenario-only. "Run this now" is operating, env-policy gated.

## 14. Compilation — non-normative

Entry + release nodes give an export signature; parking is a return value;
what would compile: plan as data, schema as validator, node set as imports.
`wamn:flowrunner@0.1.0` is a spike.

---

## 15. Persistence

**15.1 `runs`** — existing verified columns plus: the parent triplet
(UNIQUE-WHERE + all-or-none CHECK), `invoke_depth`, `catalog_id` /
`catalog_version` / `attachment_id`, **`registration_id`** (event runs; the
registration content hash rides the invocation context per §5.7),
`caller_outcome_*` +
`caller_released_at` (CHECKs: released ⇔ kind; kind ⇒ body; responded ⇒
release node), `response_deadline_at` / `run_deadline_at` (ordering CHECK),
`waiting_child_*` + `wait_generation`, `cancel_requested_kind/at`,
`invocation_context` + `admission_context_version`, `platform_revision`,
`cancel_kind` / `terminal_reason` (+`effect-uncertain`). `trigger_source`
gains `http`, `internal`, `studio`. `result_json` is diagnostic; the caller's
answer is `caller_outcome_json`.

**15.2 Execution projection and effect ledger** — `node_runs` is a mutable
execution projection; it is not effect authority. The separate immutable
`effect_attempts`, `effect_attempt_dispatches`, and `effect_attempt_outcomes`
relations are
run-state-owned. An attempt is unique for
`(tenant, run, frame_id, local_node_id, occurrence)`. A dispatch repeats those
coordinates, has a composite FK to the attempt, and has the same exact UNIQUE
key, so first-insert-wins. An outcome belongs to the exact attempt. The landed
immutability trigger covers all three relations; pure occurrences create no row.

**15.3 Definition plane** — `flow_artifacts` (DB-immutable, including each
resolved node's `pure | effectful` policy);
`release_flows`; `attachments` + `sources`; `catalog_heads`;
`attachment_activation` + `_events`; `invocation_admissions` (§6.2 shape);
`run_queue.lease_generation`; `cron_anchor` generation columns. RLS/seed
version keys are 2B; `event_registrations` unchanged (deferred tranche).

**15.4 Ownership** — the invocation service and the dispatcher sweep join
`runs` writers in `state-owners.json`; new objects for artifacts, members,
heads, activation, admissions; the catalog↔run-plane seam named as a topology
commitment; `flow-http`'s package-roles entry lands with the code.

**15.5 Canonicalization** — SHA-256 over RFC 8785 for `graph_hash`, the
client fingerprint, `caller_outcome_hash`, definition hashes, the
registration hash (each preimage versioned where it can evolve). Query
canonicalization: percent-decode → canonical re-encode → sort by key with
repeated values in wire order; empty distinct from absent. Collision
resistance is a stated cryptographic assumption.

---

## 16. Fixture refresh (there is no migration)

One flow schema, one struct, `deny_unknown_fields`. By hand, in the same
change series: retained fixtures edited to the entry-node shape; stored scenarios and
the event-input golden test updated with the shape; dev databases dropped and
reprovisioned from `deploy/sql`; `register_flow`/copy-env rewritten against
`flow_artifacts`.

---

## 17. Measurement plan — normative gate

Metrics: commits, SQL statements, rows, WAL bytes, latency percentiles,
throughput, recovery latency. Scenarios: pure request (both respond shapes);
one Postgres effect; one HTTP effect; a sent-without-outcome effect; a child;
burst; post-release continuation; a cancellation race; the Appendix B
saga; the row-fence micro-bench. Sequence: baseline → fenced single-shot writes
→ inline-with-claimed-row → sweep variant only on a miss. Budgets
`DEFERRED(owner)`. Exit: R6 remains the sole execution model if the budgets
pass; a miss triggers a PostgreSQL performance investigation, never a second
source of truth.

---

## 18. Open decisions

1. Execution locus (needs §17). 2. Output/error schemas in the first
release. 3. Run-status surface scope (required; scope open). 4. Waiter
transport. 5. `message`/`blob` timing. 6. The `DEFERRED(owner)` constants.

---

## 19. Implementation sequencing with acceptance criteria

Conventions: SR27/SR28 entries land with code; findings close via commit
messages; re-pin at each phase start; the standing rule applies.

**Order:**

```
1 → 2A → 3 → 4 → 2B → 5 → 6
```

**The first black-box product milestone (Phase 4's headline):**

```
real HTTP request → current route/auth definition → durable admission
→ request entry → standard-node execution → respond or fail
→ exact caller outcome
```

Nothing gates that milestone except what it exercises.

### Phase 1 — pure flow model

| Positive | Negative |
|---|---|
| the flow schema (entry nodes, `fail`) parses and validates; F1–F4 fixtures in the new shape pass; F1's plan matches f1proof **business-result parity** — the old proof's malformed-request audit assertions are rewritten, since malformed input is now a 400 with **no run** | every P-predicate counterexample rejected with its named error; the unreachable-node test flipped to error; **an all-`fail` request flow rejected (`no-response-node`)** |
| the work-list drain graph passes | unknown fields rejected (`deny_unknown_fields`) |
| canonical bytes stable across key order/whitespace/platform; SHA-256 digests of property-generated unequal fixtures unequal | collision resistance stated as an assumption, never a tested claim |
| request/event input structs round-trip; the golden test documents the current shape in the same commit | absent event images omitted, never `null`; a retired cron entry is refused |

### Phase 2A — the minimum callable-flow spine

`flow_artifacts`; `release_flows`; `catalog_heads`; `http`/`internal`
attachment definitions + auth/caller-policy sources;
attachment activation + events; `invocation_admissions`;
`run_queue.lease_generation`; the run-state transitions module; the coarse
promotion rule; dispatcher and materializer converted to `admit()`.

| Positive | Negative |
|---|---|
| publish → artifacts + members + `catalog_heads` in one transaction; re-publish idempotent; injected failure at every step → zero partial state | differing content for a `(flow, version)` → `flow-version-content-conflict`; `UPDATE flow_artifacts` rejected **by the database** |
| the transitions module exposes the named transitions; all three producers admit through it under the head lock | a direct-SQL run insert outside the module fails **static conformance** (the stated alpha strength) |
| carry-forward + tombstones + activation events proven | tombstoned ID reuse rejected; stale base rejected; non-transactional DDL rejected at preflight |
| §5.4a: publication refused while a nonterminal run is pinned; succeeds after wait or cancel | no impact-classification path exists to bypass the block |
| fence + typed results proven at the SQL level (race harness on the module) | `FenceLost` → zero subsequent reads/writes |
| event admission uses the retained event entry shape | a retired cron entry is refused |

### Phase 3 — engine and runtime

| Positive | Negative |
|---|---|
| entry-reserved semantics; release-and-continue; pure occurrences write no effect facts; each effectful occurrence writes one attempt before send and acquires at most one dispatch; **frontier exhaustion terminalizes an event run `completed` through §9.9, including across an unwired completion port** | **sent-but-lost**: the sink observes exactly **one** effect, the worker crashes before the outcome write, reclaim yields `effect-uncertain` and the sink count stays at one — the crash-before-send variant observes zero effects and still never redispatches; **this is a Phase 3 gate, not POC Wave 2** |
| **run context**: a `ctx` write replaces the in-memory document (a later write without `merge()` provably drops prior keys); an effectful node's context-resolved params land in `attempt_input_ref` | a child run starts with **empty** context regardless of the parent's; error-port emissions never mutate context; no durable context restore API exists |
| attempt intent atomic with lease renewal; `attempt_deadline_at` enforced pre-dispatch | the paused-original pair: (a) deadline lapsed → a new claim performs no effect; (b) cancellation during a live attempt → seizure deferred, `cancel_requested` persisted and applied at attempt end |
| deadline cancels an executing guest within the bound; sweep cancels all five orphan scenarios within the stated bound | interrupted instance disposed, never reused; a `started` attempt never reclaimed early |
| capture mode neither authorizes nor blocks recovery | post-cancel, the invocation-scoped credential no longer resolves |

### Phase 4 — invocation service, ingress, the vertical slice

| Positive | Negative |
|---|---|
| the black-box milestone passes end-to-end | in-flight duplicate → 409, no second run row |
| five release paths → exactly one caller outcome (race harness, real runtime) | a cancelled run rejects a late `respond` |
| **the §6.2 lookup order proven**: authentication uses the route-selected policy; a released outcome is served from a **disabled** attachment | a changed definition hash → `idempotency-scope-changed`, never a second run |
| retry across a promotion (attachment unchanged) returns the stored outcome | a removed route ends recovery where §6.2 says |
| pure path measured at two commits | body never pre-allocated from a caller-supplied length; flow-http sustains the adversarial suite (prevention, **not H9 closure**) |

### Phase 2B — release normalization (after the slice)

RLS + seed release membership; retire/purge lifecycle; carry-forward
refinements; cache generations **when a cache exists**; fine-grained
promotion impact **when the coarse rule demonstrably hurts**; the event
tranche when it starts. Each item arrives with its own acceptance table
under the standing rule.

### Phase 5 — invoke-flow and sagas

| Positive | Negative |
|---|---|
| parent recovery finds exactly one child; wake at release in one transaction | post-release child failure alters nothing in the parent; stale `wait_generation` rejected |
| pinned-release resolution; parent cancellation seizes the child's generation pre-release | a child is never cancelled after its own release |
| Appendix B: all cases | no effect from the stale original; no duplicate effect from the reclaimer |

### Phase 6 — measurement gate

| Positive | Negative |
|---|---|
| budgets met on both respond shapes; saga amplification within budget; recovery latency within bound | sweep variant not adopted unless the default misses; nothing optimized before the gate; R6 survives only via passing budgets |

---

## 20. Revision history

Revs 1–11: entry model → release semantics → durable protocols → catalog
release → distributed authority → earn-its-keep → flattening → deferred
seizure and realizable fencing. Rev 12: the greenfield cut (migration
machinery deleted; fixture refresh; surviving authority fixes). **Rev 13:
the vertical-slice cut** — the §6.2 lookup order corrected (route selects
the attachment which supplies the auth policy; disabled definitions
resolvable for recovery; disablement never blocks outcome retrieval); the
coarse promotion rule replaces the impact engine (no nonterminal pinned
runs — wait or cancel); Phase 2 split into 2A (spine) and 2B
(normalization, after the slice); idempotency recovery narrowed to an
unchanged `(attachment_id, definition_hash)` with `idempotency-scope-changed`
(deleting the preimage spec and `auth_source_hash`); caches pay their own
way (views are the authority; generation machinery arrives with the first
cache); the freeze ceremony removed (current-shape golden test); `respond`
range `200..=599`; the transitions-module guarantee stated at its honest
alpha strength (static conformance); saga acceptance scheduled after the
HTTP slice; the black-box milestone named as Phase 4's headline. **Rev 14 (the POC-plan
review round): `fail`'s authored `{code, message?, status?}` contract
restored (default 400, range 400..=599 — lost in the rev-10 flattening);
§9.9 natural completion defined (empty frontier + no failure +
no-caller-or-released → fenced `terminalize(completed)`); P6 re-based on
Resp (respond nodes in S) so an all-`fail` request flow is rejected, and
event predicates declared inapplicable rather than S "empty";
`admit()` given producer-specific identities with the event dedup scope
defined as `(tenant, registration, seq)` via the existing idempotency-key
index; the invocation interface completed with `invoke-request`, a typed
pre-run `rejected` variant, and the `cancel(run-id)` handle; f1proof parity
restated as business-result parity under the new 400-no-run behavior.**
**Rev 15 (the r2 review round + the run-context decision): every completion
emission becomes `{output, ctx?}` — universal context reads via the
`context()` expression function, any node writes via its `ctx`
expression/envelope field, a write replaces the document, merge is authored
via the `merge()` builtin, error emissions never write; durability rides
`state_json` with pure-node reconstruction (a historical design withdrawn by
`wamn-0h0g.4.5`) and effectful attempts recording context-resolved params; the
invocation handshake split into
`begin → admitted{run-id} | rejected` then `wait`/`cancel`, with
`invoke-request` carrying the preflight expectations and fingerprint; the
producer admission table extended to lifecycle (definition check and initial
queue state per variant; events record the live registration hash and start
unclaimed); child revocation narrowed to creation (recovery of an existing
child never re-authorizes); the custom-node `purity: pure` manifest
assertion defined; `delay`-in-S narrowed to request entries;
`runs.registration_id` added; the sent-but-lost proof named
a Phase 3 gate.** **Rev 16: `failed`/`cancelled` results carry the stored
`caller_http_status` (`failure = {status, error}` — an authored `fail 400`
now reaches the wire as a 400); `wait` made bounded
(`wait(run-id, timeout-ms) → option<invoke-result>`) so the adapter can
observe disconnects between waits — a blocking wait could never see the
event it reacts to; the three residual "claimed queue row" statements
rewritten to producer-determined initial state; `service` mode defined
normatively (callee service identity, caller lineage retained for audit).** **Rev 17: every failure kind given
its platform `caller_http_status` mapping (500 / 504 / 499 table in §8 —
the interface required a stored status only authored `fail` produced);
envelope `message` made optional to match `fail.message`; §6.1's A/B stages
restated producer-shaped (event resolution is registration-based; full
auth/body/mapping B is HTTP's).** **Rev 18: stage C's residual "recheck activation/revocation"
made producer-specific (events check-and-record the registration hash — no
attachment exists to activate).** **Rev 19, amended by `wamn-0h0g.4.9`:
standard and custom nodes share one `pure | effectful` policy. Pure occurrences
write no effect facts; each effectful occurrence writes one immutable attempt,
at most one dispatch, and an outcome when known. A sent attempt without an
outcome becomes `effect-uncertain` and never sends again. HTTP method, mutable
configuration, endpoint behavior, and capture cannot strengthen dispatch.**

---

## Appendix A — worked example

The `erp-sync` flow: `request` entry with `input-schema`; S = `{in, lookup,
accepted, not-found}`; release at `accepted` (`202`); caller-gone
`deliver → record` with `error → gave-up` (`fail`); all predicates pass; a
post-release exhausted retry ends the run `failed` after the caller saw
`202` — visible only through the run-status surface (§18.3). Attachments
(partner / UI / internal) are release members with activation rows; the
nightly variants are unavailable in the MVP; scheduling cannot be smuggled in
as a second or wrapper entry.

## Appendix B — saga acceptance (after the slice)

`order-saga`: `reserve-stock` → `charge-payment` → `schedule-shipment`, each
an `invoke-flow` to a `request`-entry child behind an `internal` attachment;
child effects each dispatch at most once; compensation via error edges into
`saga-failed` (`fail`); success releases `200`, post-release `record-audit`.
Cases: (1) mid-saga crash — occurrence-keyed recovery, no duplicate child;
(2) **promotion-while-parked under the coarse rule** — publication is
refused while the saga is parked, succeeds after cancel-or-drain (the
simpler rule, proven directly); (3) revocation-mid-saga — disable drives
`callee-revoked` → compensation; (4) stale-executor wake — `FenceLost`, zero
reads, zero writes; (5) effect authority — the paused original initiates
nothing; sent-then-lost yields `effect-uncertain` with no duplicate from the
reclaimer.
