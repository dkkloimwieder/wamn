# Execution model revision — component palette, user wiring, pooled execution (DRAFT for review, rev 4)

Status: DRAFT · owner-directed 2026-08-16 · rev 4 adds the
developer data-access ruling (sqlx), the generative-API pattern,
and the WASI P3 grounding (the pinned wamn/2.7.0 runtime carries
`wasmtime-wasi p3`, `component-model-async`, and the pooling
allocator — verified at the tag) ·
amends the charter's execution model, authoring surface, and parts
of the deployment amendment · tracker `wamn-0h0g`.

## Rulings proposed

**R1 — audience split.** Developers author **components** — typed
WIT operations with declared ports, parameters, effect posture, and
connection requirements — published into a per-catalog **library**
(OCI, digest-pinned, import-audited). Non-technical users wire
library components into flows dynamically, in production, through
the studio. Users own composition; developers own logic. This is
wasmCloud's own division of labor: the component with its WIT
interface is the unit of development and distribution; composition
is typed linking, not shared code.

**R2 — durability default.** At-least-once delivery + producer
idempotency + OTel as the record. The crash floor (classifier,
effect ledger, effect-uncertain, operator terminalize) **shelves as
the future premium durable tier** — landed code behind a class
gate, not deleted. R2 depends on R1: "document idempotency" is a
contract available to component developers, not to the wiring
audience.

**R3 — two-tier deployment.** Platform artifacts (components,
runtime, charts) ride the ratified OCI + GitOps + rollout model.
**Wirings are data**: versioned, gated, hot-activated by pointer
flip. Upstream precedent: wasmCloud links components at runtime via
configurable link definitions — wiring-as-hot-data is the native
shape, relocated from lattice state into our gated tenant store.
Divergence annotated: wirings are tenant rows, not CRDs — etcd and
rollout cadence are wrong for per-user minutes-scale churn.

## Execution mechanics (how a flow runs)

**What precisely retires, survives, and moves.** *Retires:* the
node language and expression configs, standard-nodes as language
builtins (they return as ordinary palette components), the
flowrunner as a monolithic guest, execution plans + the
deterministic compiler, frames/call-flow, per-node durable facts,
capture. *Survives rehosted:* the flow-engine's **graph walk** —
frontier ordering, port routing, error-edge semantics — moves into
the host-side **router** largely as-is (it was always the smallest
part of the engine; the language around it was the bulk).
*Survives unchanged:* the queue, plugins and effect authority
(binding → active generation), the gate, ingress begin/wait.
"Retire the interpreter" in rev 2 was wrong wording: the *language*
retires; the *walk* is rehosted native.

**The router.** A host-native module (in the executor/host process,
~1–2k lines, mostly relocated engine code). Per delivery: resolve
the wiring (active version from the env-hot store, cached by
version) → walk the graph in frontier order → per node, acquire a
**pooled instance of that node's component** and invoke the
operation over the existing typed WIT seam → route outputs by port
→ terminal `respond`/`emit`/discard. Budgets become a **hop limit**
per delivery (the dispatch-budget shape, no frames to bound).
Effects execute *inside* developer components against bound
connection generations through the landed plugins — the authority
chain is unchanged minus the ledger writes.

**Pooling, restored in full.** The upstream pooled allocator
(#5398) is adopted platform-wide: pre-initialized modules yield
**fresh instances at microsecond cost**; instance *reuse* (linear
memory persisting across inputs) stays off until windowed state
ships with explicit affinity — the fork's `g2br.16` kill-switch
narrows from per-host to per-capability, mechanism kept. Pools are
keyed **per component digest, shared across every wiring and tenant
flow using that component** — fifty wirings using `csv-parse` share
one warm pool; this cross-wiring amortization is the decisive
argument for the router over compile-time composition (below), and
it is where wasmCloud's demonstrated 10⁴–10⁵ concurrent-invocation
density is earned. Per-host pool sizing is a chart value; audit B's
fresh-instance conformance test re-runs under the allocator. The
pinned runtime carries the substrate verified at the tag:
`wasmtime-wasi` **p3**, `component-model-async`, the pooling
allocator, and gc — the spec's execution model is configuration of
the pinned fork, not a future dependency.

**Alternative recorded — compile the wiring (WAC).** wasmCloud's
native composition tool can link a wiring's components into one
composite component per wiring version: zero host crossings on
internal edges, typed at compose time. Rejected as the default:
per-wiring composites **fragment the instance pools** (defeating
cross-wiring warmth), mint an artifact per wiring edit (churning
the hot path R3 exists to keep light), and bury the per-edge tap
points the studio live-view needs. Recorded as the demand-gated
**fusion optimization** for proven-hot pure pipelines — the router
chooses per-edge invocation now, composition later where profiling
names the need. Per-edge host-crossing cost is the wit-boundary
cost already accepted as noise against any effect; for pure
segments it is the price of shared pools and taps.

## Ingress paths (three, exhaustively)

**1 · Hot HTTP routes.** The landed routing plugin resolves the
attachment → router runs the wiring inline on pooled instances →
`respond`. No run row, no queue row, no Postgres in the path.
Concurrency = pooled instances; backpressure = bounded in-flight
per route (chart value) then 429.

**2 · Streams.** Per-registration **durable pull consumers** on the
env's JetStream stream (landed materializer machinery, S-branch):
pull with `{max_batch, max_wait}` — registrations declare
`input: event | batch`; a batch delivers as one router run with the
ordered array. **Ack-after-completion**; failure nacks for
redelivery under the consumer's retry budget; poison dead-letters
to a capped per-registration DLQ subject (operator surface).
At-least-once in-stream, by contract. MQTT and device protocols
enter via NATS's native bridging — no new ingress tier. The
manifest registration-identity gate (`.15.95`) is unchanged: the
host checks identity ∈ active manifest before the router sees the
event.

**3 · Triggered automations.** Queue-backed (the ~700-line SQL
queue survives verbatim): admission writes the run+queue row under
the producer-idempotency key; a worker claims (SKIP LOCKED, lease)
and hands to the router; completion deletes; crash ⇒ lease expiry ⇒
redelivery — at-least-once, no classifier. `begin/wait` and
same-key-same-outcome survive for synchronous callers.

## The boundary (where durability is bought)

`emit` publishes a derived event with an author-supplied dedup id;
Class-D admission (path 3) dedups on it via the landed
producer-idempotency machinery — at-least-once in-stream becomes
exactly-once-admitted at the boundary. Telemetry-rate ingest never
meets Postgres: NATS → pooled instances → boundary emit. The
premium durable tier, when sold, slots in at path 3's claim
(classifier + ledger re-enabled per class) with zero changes to
paths 1–2.

## Developer surface — data access and generated APIs

**sqlx is the de-facto standard for Postgres access in components
(ruling).** Developers write `sqlx::query_as!(...)` — compile-time
checked against the **dev environment's applied catalog** (`cargo
sqlx prepare`; the offline query cache commits with the component).
The build records the catalog version the queries were prepared
against; the release closure's `catalog ≥` rule then binds a
compile-time fact, and struct/schema drift is a dev-time gate
failure, never a prod surprise. Upstream precedent: sqlx runs
in-component today (wasmCloud ships it as a CI fixture; Tokio
`current_thread` on the WASI socket stack), and P3's native async
is its intended substrate.

**Annotated divergence — the import seam, not raw sockets.** The
native path gives guests `wasi:sockets` + allowed-hosts and lets
sqlx speak wire protocol directly. We instead ship a thin **sqlx
driver backend over `wamn:postgres`** (~200 lines; sqlx's
multi-backend traits exist for exactly this), now implementable as
a true async WIT surface under `component-model-async`. Why we
diverge: host-injected **credential generations** (authors never
see connection strings), RLS session context set by trusted code,
a span per effect, and per-attempt authority — the authority model
is the product; network-policy-grade control is not it. Raw-socket
sqlx is recorded as the rejected-native alternative. Connection
requirements ride the component model's own **`implements`**
naming (on by default in the pinned runtime, #5435): a declared
store alias (`appdb`) resolved per workload against the host
plugin registry — upstream's vocabulary for our binding grain, and
their stated constraint (bindings static, names known ahead of
time) *is* our admin-bound-connections model.

**Generated APIs — one pattern, three grammars (ruling).**
Authoring automation **emits ordinary gated artifacts**; nothing
generated is gate-exempt or reflection-served. The flagship:
`generate crud` walks the applied catalog and emits, per relation,
five wirings (`http-entry → entity.<op> → respond`), their route
attachments, and auto-written cases (create→get roundtrip,
list-filter, update, delete→404) — arriving gate-green by
construction. The **`entity` component** (standard library,
platform-authored) is the wirer's generic data node: relation and
filter spec as declared parameters, RLS enforcing row/tenant scope
via the landed policy compilation. One generic component means one
warm pool shared across every CRUD route and wiring — generation
is document-emission, instant, closure-light; schema evolution is
re-run → re-gate → flip. Per-relation typed codegen (real WIT
records per table) is the demand-gated alternative — types at the
cost of N regenerating components and pool fragmentation; same
trade shape as WAC fusion, same resolution. The three grammars,
stated once: **components consume the catalog through sqlx over
the import (compile-time checked); wirings consume components
through typed ports (gate-time checked); external consumers hit
generated routes (runtime, RLS-scoped)** — one schema, three
access grammars, each bound at its own time. Customization is
graceful: generated wirings are ordinary wirings — insert a
palette node ahead of `entity.create`, or swap one route to a
hand-authored controller; regeneration is emit-if-absent.

## Observability

OTel end-to-end: traceparent from ingress, one span per component
invocation and per effect, per-wiring/registration metrics
(throughput, error, ack lag, DLQ depth). The studio **live view**
taps at router edges — ephemeral payload previews on a bounded
subject, possible precisely because the router owns every edge
(the composition alternative would bury them). No durable node
facts, no `get-run` for the default tier.

## Promotion and the release closure

The release is the deployable closure; promotion is four rules.
**Schema:** the release names the catalog version it was gated
against; target's applied version must be ≥ it, else
`migrate-catalog` (additive-or-refuse) first — one integer
comparison; component-vs-schema correctness is the **gate's**
jurisdiction (cases run against the real applied catalog at
authoring and at promote-time re-gate), never a packaging
precondition. **Components:** the release lists `(component,
interface-version)` + digests; target library must cover at
compatible WIT interfaces, else deploy pulls the digests first.
Late binding holds within an interface version. **Integrations:**
requirements bound in target; generations never travel.
**Wirings:** re-gated at target against target palette + bindings
(dev green proves logic and shape, not prod endpoints — stated in
the promote UX), then pointer-flipped; the promote verb records one
provenance fact (wiring hash, source env, both gate report ids,
principal, timestamp). Deploy order: migration → components →
wirings → flips; each step idempotent; mid-way failure leaves
additive schema and inactive wirings — resumable, no saga.
**Two speeds, deliberate:** rewiring within the deployed closure is
the seconds-fast flip; the packaged path runs only when the closure
grows — developer-cadence by nature (Node-RED's palette-install vs
live-wiring split).

## Compare and contrast with the current implementation

| Concern | Today (landed) | This spec |
|---|---|---|
| Unit of authoring | Flow doc: 9-node language + expressions + in-draft tests | Developer components (WIT ops) + user wirings; same in-draft test contract |
| Graph execution | flowrunner guest interprets plan; frames, budgets | Host router walks wiring (engine's walk rehosted); hop limit; pooled per-component instances |
| Instantiation | Per-run instance, pooling off | Pooled allocator platform-wide; per-digest pools shared across wirings; reuse deferred |
| Hot path | Every trigger: run row + queue + claim + ledger | Routes/streams: zero Postgres; automations: queue only |
| Durability | Crash floor per effect | At-least-once + idempotency + DLQ; floor shelved as premium tier at path 3 |
| Streaming | None (run-per-event) | JetStream pull consumers, batch input, ack-after-process, DLQ, MQTT via NATS bridge |
| Observability | node facts, capture, get-run | OTel spans/metrics + router-edge live view |
| User-artifact deploy | draft→plan→OCI→CM→rollout | Gated wiring rows, pointer flip; platform tier unchanged |
| Dev→prod | Per-flow pipeline | Release closure: catalog ≥, digests pulled, bindings verified, re-gate + flip, provenance fact |
| wasmCloud usage | Sandbox + epoch; pooling off; composition deleted | Components, OCI, typed links, pooled allocator as designed; WAC recorded as fusion option |

## What retires / shelves / survives (vs the measured 61.7k)

Retires (~11k + test mass): flow-model, standard-nodes-as-builtins,
flowrunner guest, plans + compiler, frames, capture, node facts.
Rehosts (~1–2k of flow-engine's walk → the router). Shelves
(~4.2k behind the durable-tier gate): classifier, effect ledger +
writer generations, effect-uncertain, terminalize. Survives: the
tenancy/Postgres half, connections/authority, gate + evidence +
command ledger, queue, ingress, event spine, deploy/operator,
inventory. `deploy-simplification` redirects: manifest/weld/OCI/
gate/tenancy waves stand; flow-language waves stop.

## Tradeoffs, owned

Supply chain returns as the load-bearing floor (author executables:
import audit, digest pinning, eventually signing). At-least-once
means duplicate component effects under redelivery — idempotency by
authorship + boundary dedup, the SQS/Sidekiq contract, appropriate
to the developer audience (R2 ⇐ R1). Per-edge host crossings on
pure hot pipelines — the price of shared pools and live-view taps;
WAC fusion is the recorded escape. End-user debugging shifts to
traces + live view — weaker than durable node facts, accepted.
Two execution contracts once the durable tier sells. Type-compat
validation is shape-safety, not semantics — the seconds-fast gate
carries semantic safety, making its latency a product requirement.
Hot wiring is the one deliberate walk-back against the deployment
amendment — bounded by the gate, types, versioning, and instant
rollback. Subflows (wiring-embeds-wiring) are named-trigger future
work; initial wirings are flat. Instance reuse with affinity and
windowed aggregation name **P3 streaming maturity** (native
streams/futures for long-lived components) as their enabling
condition alongside client demand. The 2.8 fork sync carries a
**trace-seam shrink audit**: upstream 2.3+ ships cross-host trace
propagation and configurable OTel exporters — `g2br.4` drops or
thins where subsumed.
