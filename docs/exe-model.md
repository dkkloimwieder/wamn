# Execution model — scope-reduction WIP

Status: **WIP and sole current design authority** (owner-directed 2026-08-21).
Tracker: `wamn-0h0g`. [PLAN](PLAN/PLAN.md) is the non-normative ordering and
ambiguity map; completion lives in Beads and git. Documents under `docs/archive/`
are not design authority; explicitly named operational ledgers remain maintained.
This document wins every design conflict.

## Product decisions

1. **Components and wirings.** Developers publish digest-pinned, import-audited
   components with typed ports (JSON Schema), parameters, effects and connection
   requirements. Users compose them as versioned wirings. Developers own logic;
   users own composition. Tenant admission accepts only the closed platform
   capability registry plus exact WASI I/O, clocks, random and logging packages;
   it grants no `wasi:*` wildcard. MVP wiring compatibility is exact canonical
   schema-digest equality: structural subtyping would be a compatibility promise
   that cannot be withdrawn, while exact equality can be relaxed deliberately
   later.
2. **Durability.** The default is at-least-once delivery, producer idempotency and
   OTel as the record. The classifier/effect-ledger crash floor remains shelved
   behind a future premium-durability class gate.
3. **Deployment.** Platform artifacts use OCI and operator-managed hosts,
   converged by operator kubectl per deploy/README.md's runbook; a GitOps
   controller is demand-gated on a second environment or the first non-human
   convergence. Wirings are gated tenant rows activated by pointer flip, not
   CRDs or per-edit artifacts.

## Runtime target

- Retire the flow language, expression configs, standard nodes as built-ins, the
  flowrunner guest, execution plans/compiler, frames/call-flow, per-node durable
  facts and capture.
- Rehost the proven frontier walk, port routing and error-edge semantics in one
  host-native router shared by HTTP and queued execution. Each delivery resolves
  the exact wiring version from its immutable release. The router invokes each
  node and routes outputs to `respond`, `emit` or discard under a hop limit.
- Key cached wirings by tenant, package, environment, effective release, wiring ID,
  and version. Frozen candidates retain their exact admitted wiring and component
  facts. The runtime does not follow mutable activation pointers or PostgreSQL
  notifications. Activation writes, history, and validation remain in the catalog.
- Use native wasmCloud loading and dispatch for released, nested, and frozen
  candidate node calls. Native code owns compilation reuse, component linking,
  store construction, and invocation duration metrics. The Wasmtime pooling
  allocator remains enabled. Every call receives a fresh store.
- Keep `poolSize` unset or zero. Admission continues to reject positive values.
  B2 requires tenant isolation, separation of request state from warmed stores,
  `maxConcurrency = 1`, and ephemeral fallback when the pool is full.
  Its implementation and resource rules must land with the execution-model and
  ledger amendments. No benchmark is a prerequisite for B2.
- Keep complete admitted component facts separate from compiled bytes. Each
  fact owns its immutable statements and host bindings. The native compilation
  cache uses the digest of bytes that WAMN checks before loading.
- Register request authority only after native initialization. A unique scope
  carries the caller, claims, statements, causation, and effect authority.
  The store's active identity names that scope during the call. Cleanup revokes
  the scope and restores the native component identity on every exit.
- Keep WAC composition only as a demand-gated fusion optimization for measured
  hot, pure pipelines. The default preserves fresh stores and router-edge taps.

One `wamn-execution-host` driver serves HTTP and queued execution through the
uniform `wamn:node` seam and native fresh-store dispatch. Services depend inward
on execution-host, runtime, catalog and router. `to_port` is enforced whenever a
target has multiple inputs and may be omitted only for a single-input target.

Native alignment B (`wamn-0ct2.2`), owner decision 2026-09-10, requires one admitted provider for each operation interface imported by the admitted closure.
The fully qualified interface token includes its version. Admission retains exact package and artifact provenance without per-dependency provider selection.
Export-only interfaces can repeat. Wiring selects palette nodes by their complete admitted facts, so their shared `wamn:node/handler` export needs no uniqueness rule.
The [B plan](architecture/wamn_native_alignment_plan.md#b-replace-manual-guest-execution-including-its-duplicate-caches) owns the native production substitution and deletion contract.

The driver retains one `NativeApplication` for its immutable released component
set. A candidate retains one application for its full traversal. Neither path
creates a native workload per wiring node. Exact repeated facts share a native
component identity. Distinct authority facts remain separate even when their
component bytes match.

One absolute deadline covers node acquisition, native initialization, execution,
and nested calls. A child cannot extend it. The host and executor use Tokio's
public `event_interval(1)` to poll timers after repeated guest yields.
Native dispatch owns guest abandonment and memory release. WAMN retains the
application owner inside the actual guest call until native work releases it.

Native host callbacks restore their invocation trace through the shared
`invocation_trace` carrier. It holds the host span and subscriber under the
existing invocation scope. Native policy installs it after initialization and
revokes it with that scope on every exit. It exposes no guest interface,
configuration, or authorization rule.

The guest call, nested dispatch, and HTTP, PostgreSQL, and blob callbacks use
that captured context while their futures run. Synchronous callbacks restore
it only during the callback. Other host paths retain their ambient context.
Guest-supplied logging trace context keeps its existing separate contract.
The [main authenticated native proof](perf/2026.09/native-b-adoption/production-authenticated-main-001/output.log)
passes all six scenarios at `6f02d70d6b0a03b90160652df2d9c16c065010e3`.
It retains exact root, child, and host-observation parent assertions.

An application cleanup guard exists before native resolution starts. It clears
WAMN bindings and invocation scopes after a cancelled or failed load and on
final owner drop. Shutdown serializes with authority registration. Candidate
completion also calls the public native plugin-unbind API. No cleanup worker
or second runtime owns this lifecycle.

The completed B production implementation removes the manual compiled cache,
`PreparedCache`, linker cloning, `NodeInstance`, and deadline-to-epoch conversion.
The [deployed Receiving proof](perf/2026.09/native-b-adoption/production-receiving-002/receiving-correctness-journey.receipt)
passes at the same production source. Its receipts cover 19 histories, 129 steps,
and seven boundaries. The [production report](perf/2026.09/native-b-adoption/production-report.md)
records exact proof identities, historical failures, and workspace limits.
The owner prohibits benchmarks in this wave. B makes no performance claim.

### Composition: an edge carries the route envelope

RULED `wamn-362o.42` (2026-09-05). A wiring edge carries one value of one
schema, and the authoring gate compares the two port-schema digests of every
edge for equality. The value on an edge is the **route envelope** — the array
of `{request_id, value}` / `{request_id, error}` items the entry operation
emits and the route answers with — so a palette node declares `{"type":
"array"}` on both ports, byte-identical to the entry's, applies itself to each
item's `value`, passes error items through untouched, and enriches rather than
replaces (the route answers with what the last node emits). Wiring pointers
resolve against the item value. `label-render` and `blob-put` are the first
two nodes written to this rule; the first composed edge the gate ever
evaluated refused on the array-versus-object mismatch that preceded it.

RULED `wamn-362o.46` (2026-09-05). A node that must **await** an async
capability import exports `wamn:node/async-handler@0.1.0` — the same `run`
shape typed `async func` — and lifts it async; sync nodes keep `handler`. The
component model permits the `async` canonical option only on an `async func`
type (the validator refuses an async lift of `handler.run`), and a
synchronously-lifted export cannot block on an async import (blob-put's first
execution trapped on exactly that). The router dispatches on whichever handler
interface a node exports and drives both through native `GuestCall` and
`call_concurrent`. Admission admits the async lift on `async-handler` alone. Named for what it is, not
versioned: nothing outside this tree consumes the node ABI. blob-put is the
first consumer; streaming capabilities and MQTT remain future consumers. The
P3 HTTP shell exports `wasi:http/handler@0.3.0` separately from this node ABI.
Its WAMN routing, authentication, and delivery imports remain synchronous.

Noted as future exploration, not planned: a **router fan-out** that delivers
the envelope's items one at a time and reassembles after the last node (its
own design: ordering, partial failure, the reassembly point), which would let
palette nodes stay per-item; and **per-item outcome reporting** for nodes
whose work can fail per item (`blob-put` fails the emission as a whole
today).

### Declared served responses

An admitted registered operation can declare a `committed-result-schema` for
results that it returns only after commitment, including an unchanged replayed
result. A wiring can declare a separate `response` schema for its named Respond
terminal and select one `committed-result` node. These declarations do not change
the schemas on wiring edges.

The router retains a selected result only when the actual output matches that
operation's admitted commitment contract. A second successful visit makes that
single-result evidence ambiguous. The existing first terminal verdict still
stands when later work fails.

If later work fails before a terminal verdict, the served error carries only
`committed_result` and `failed_outcome`. The failure preserves its existing code,
message and operation when present. One observed failed capability attempt adds
its existing effect outcome. Missing or ambiguous observations add none, and a
pure label failure does not invent an effect outcome. This evidence lives only
for the current delivery, without transaction tracking or stored node history.

The generated client validates the declared partial response and its request
identity before it reports partial completion. A malformed response or absent
commitment evidence leaves the outcome unknown. Neither partial completion nor
uncertainty grants replay of a composed route.

## Ingress and durability

1. **Hot HTTP:** attachment → router → response, with no run or queue row.
   Per-route in-flight bounds refuse excess work with 429.
2. **Streams:** native durable pull consumers deliver one event or an ordered
   batch for each registration. Acknowledgement follows completion, retries stay
   bounded, and the broker records exhausted or terminated deliveries as advisories.
3. **Automations:** admission atomically writes run and queue rows under a producer
   key; claim/lease hands work to the router; expiry redelivers. `begin`/`wait`
   and same-key/same-outcome remain.

The reviewed [C source checkpoint](perf/2026.09/native-c-advisories/source-checkpoint-001/handoff.json) under `wamn-0ct2.7` uses native `wasmcloud:nats/jetstream@0.1.0` through the named `events` binding.
The materializer is platform infrastructure.
The checkpoint removes WAMN's custom delivery and settlement resources and its payload dead-letter queue.
WAMN retains exact release-registration checks, consumer preparation and drift refusal, derived publishing, and the scheduler doorbell.

The host reads native binding configuration from `--materializer-nats-binding-file` or `WAMN_MAT_NATS_BINDING_FILE`.
The private file carries the native server, credentials, inbox prefix, and stream and subject grants.
`WorkloadConfigPolicy::Deny` prevents workload configuration from replacing that binding.
Each environment uses separate materializer credentials for its allowed stream and exact durables.
Broker permissions allow their information, pull and acknowledgement subjects and a private inbox, without stream creation, consumer creation, or event publishing.

Trusted application declarations supply `WAMN_EVT_ORG`, `WAMN_EVT_PROJECT`, and `WAMN_EVT_ENV` to the host and executor.
These event coordinates remain separate from tenant identity and database-project authority.
Direct runtime, CDC and operator clients retain `WAMN_EVT_NATS_URL`.
When authentication is configured, `WAMN_EVT_NATS_USERNAME` and `WAMN_EVT_NATS_PASSWORD_FILE` are required together.
Password bytes stay in private files and host memory, outside workload configuration and recorded commands.

This checkpoint establishes source changes and focused test results only.
Live correctness, broker authority, delivery pressure, and the integrated retained workspace test run remain pending under `wamn-0ct2.7`.
The scope of observer access to shared advisory metadata still requires the owner's decision.

`emit` carries an author-supplied dedup id; automation admission deduplicates it.
The queue does not survive verbatim: classifier/effect-attempt predicates must be
class-gated out of the default tier, including the trusted HTTP-effect check.
An explicit system env-policy class converges into a project-local admission row;
admission freezes it in `runs.durability_class`, and claims read only that carrier.

### Premium durable shelf contract

- A pure occurrence writes no effect-ledger row.
- An effectful occurrence has one immutable write-ahead attempt and at most one
  immutable dispatch fact; exact retries are no-ops and different facts refuse.
- The first successful dispatch insert is the sole wire-I/O permit.
- A sent attempt without a recorded outcome is `effect-uncertain`; it never sends
  again. Admission idempotency selects the existing run and never licenses effect
  redispatch.
- There is no success assertion, continuation, bulk selection, successor attempt
  or silent re-execution.
- A configured `durable` class does not imply protection the writer does not
  supply. The class selects a protocol; it does not make an unprotected write
  safe.

### The three effect contracts

The effect surface holds three contracts, and only three. None of them is a
ledger.

1. **The capability inventory.** What an admitted component may do. The posture
   is the component's own imports union the posture of its declared operation
   dependencies, derived at admission from the closure the validator already
   walks. A component with an empty inventory is not effect-free if its closure
   reaches an effectful operation.
2. **The effect observation.** What the platform says happened. A span carries
   the originating wiring position and the executing package, component and
   operation, and an outcome. The six outcomes are refused before dispatch,
   responded, timeout, cancelled, effect-uncertain and response-lost. A timeout
   names a deadline that really elapsed. Effect-uncertain is the state the
   premium durable shelf contract above names. The two are one vocabulary and
   not two. The platform sent an attempt and recorded no outcome for it.
   Response-lost is the different state where the far side acted and its answer
   never arrived. The remedy for response-lost is to read the result again. The
   remedy for effect-uncertain is never to send again.
3. **The durable protocol.** The premium durable shelf contract above. It is
   **shelved**: it is specified, it is not the default tier, and no work depends
   on it today.

### Outbound HTTP reuse

The owner retains WAMN's pinned-address HTTP transport and permits bounded connection reuse under `wamn-ctc8.16`.
The public native connector does not accept WAMN's approved peer before dispatch.
The transport uses Hyper directly without an upstream patch or a second production HTTP path.

One process-owned transport serves the invocation-local plugins.
Each request still authorizes its invocation, release, binding, credentials, and destination before selecting a reusable client.
The client separates tenant, project, environment, connection instance, binding, credential generation, approved peer, and TLS identity.
Caller claims, credential headers, bodies, and trace context belong to individual requests, not retained clients.

Bindings and credential generations share the quota for their logical connection.
Retained clients and sockets remain charged while active, idle, or draining.
The transport also bounds concurrent requests, headers, bodies, and total duration.
Full capacity refuses before dispatch without a waiting queue.
The [HTTP report](perf/2026.09/ctc8-16-http-reuse/README.md) records the initial limits and proof gaps.

The existing effect outcomes remain unchanged.
The transport does not retry requests internally.
A timeout or cancellation does not prove that the remote operation did nothing.
The transport change does not alter nested-operation authority or admit standard WASI HTTP.

Under `wamn-ctc8.33`, nested HTTP and blobstore effects authorize the executing component and operation.
The host retains the original wiring owner, root component, and caller across nested calls.
The released root must declare the exact dependency path to the executing operation.
Release membership alone grants no access.
Candidate execution retains its frozen bindings and still refuses nested calls.

## Data, identity and generated APIs

Package application rows are tenant-scoped by database residency, not by column.

- `wamn:postgres` remains the unchanged credential-hiding WIT boundary: guests
  receive no socket or credential, and `WamnPostgres` is not aliased to a built-in
  sqlx driver. The complete application SQL corpus—generated CRUD, lock and
  mutation SQL plus authored named query and projection SQL—has a native
  `sqlx::Postgres` verifier sibling on the exact effective schema, while Wasm
  executes byte-identical SQL through runtime-checked `WamnPostgres`;
  tenant/developer and workflow-editor arbitrary SQL remains runtime-checked.
  See the [sqlx data-access specification](sqlx-data-access-spec.md),
  [base-application POC](poc/wamn_base_application_poc_revised.md), and
  [receiving scenario](poc/wamn_receiving_layered_application_poc_scenario.md).
- WAMN retains one PostgreSQL implementation because commands require exclusive
  held-session transactions. A held session keeps one connection throughout a
  transaction. The host keeps the admitted statement set and exact SQL for
  execution, commit, and rollback. An armed cancellation guard destroys an
  unfinished connection instead of returning it to the pool. Native substitution,
  a parallel read backend, and new database machinery are outside this decision.
  See [native alignment F](architecture/wamn_native_alignment_plan.md#f-retain-one-wamn-postgresql-implementation),
  [statement resolution](https://github.com/dkkloimwieder/wamn/blob/1d38b6da38a460753d89f877ec0a0c68345a7d60/crates/platform/runtime/src/plugins/wamn_postgres/statements.rs#L229),
  and [transaction ownership and cancellation](https://github.com/dkkloimwieder/wamn/blob/1d38b6da38a460753d89f877ec0a0c68345a7d60/crates/platform/runtime/src/plugins/wamn_postgres/resources.rs#L43).
- Runtime database identities are per project-environment and tenant. PostgreSQL
  `current_user`, backed by opaque bounded role names, is the RLS input; caller-
  settable tenant GUCs retire. `wamn_app` becomes a NOLOGIN ACL role inherited by
  rotating login generations with no `SET ROLE` escape.
- Credential selection is host-owned and keyed by `(project, AuthorityClass)`.
  The closed classes are guest SQL, executor platform, callable HTTP and event
  materializer. HTTP and event admission are DB-enforced per-kind operations;
  producer kind is never trusted as a parameter.
- Generated APIs emit package-owned typed accessors and registered operations as
  ordinary gated artifacts, not one universal generic `entity` component.
  External consumers use generated registered operations after trusted
  `CallerIdentity` permission checks; the host independently selects the
  PostgreSQL identity enforced by privileges and RLS. Nothing generated is
  gate-exempt or reflection-served.
- Existing compiled RLS policies use role/user claims that the production claim
  path does not inject. That correctness defect must close before any generated
  or raw-SQL guest API bearing those role/user-RLS policies becomes reachable.
  Registered operations without such policies keep authorization host-side:
  they compare the exact operation token against the authenticated principal's
  grants and bind no caller-derived database claim (`wamn-10yt.3.2`).

**THE AUTHORIZATION PATH IS NEVER CACHED. Revocation is immediate by
construction.** Ratified 2026-09-04 after a measured attempt to cache it. A
verified token, a principal status, a project role and an operation permission
are all re-read from the database on every request, and the cost of that -- 1.76
ms of a 16 ms request, measured in `perf/2026.09/2-auth.md` -- is the price of
the property.

The rule is not about TTLs. A cache invalidated on write fails for the same
reason: the write happens in one host process and the caches live in the other
N, so every cross-process signal is asynchronous and leaves a window that is
unbounded under load. `route_authentication_live` already asserts the property
it protects -- it deletes a permission and demands 403 on the NEXT request -- and
a proof rewritten to poll for that refusal would be a proof of a race. The way
to make an authorization path fast is to make its reads cheap, which is what
reducing them from nine round trips to three did; it is not to stop making them.

The owner approved a scoped session exception on 2026-09-07 and clarified it on 2026-09-08.
The [JWT proposal](architecture/wamn_jwt_proposal.md) controls that exception.
Fresh PAT authentication still reads identity, membership or service scope, and tenant permissions on every request.
Session tokens carry signed identity and environment-role evidence for at most 900 seconds, plus 30 seconds of clock tolerance.
Hosts read tenant operation permissions fresh for every new request.
They accept public signing keys only within the configured issuer's 300-second evidence window.
Private signing keys stay with the separate `wamn-identity` authority and its system database.
The fresh-only restriction belongs to each registered operation and survives nested calls.
An authored `fresh_only: true` becomes `fresh-only: true` in admitted component facts and the released manifest.
Omission means false, and the manifest format remains 1.
Admission refuses a mismatch between the authored operation, its generated contract, and its component declaration.
Each operation boundary checks the original caller's exact permission before its credential kind.
A permitted session caller receives HTTP 403 with `fresh-credential-required` and the exact operation identity.
The host does not retry that refusal or repeat earlier committed work under a PAT.
Production session routes remain disabled until the two-host and fresh-only proofs pass.
The owner permits session routes in disposable deployments for those proofs.
`wamn-ctc8.15.3` owns host session admission, and `wamn-ctc8.15.4` owns fresh-only enforcement.

Per-tenant roles imply pools per credential. Connection multiplication is the
pooler trigger, not an alternative identity model.

## Release, promotion and observability

- A release closes over exact `(package_id, package_version)` memberships,
  component/interface digests, bindings and wirings. `apply-package` is the sole
  migration applier. Promotion verifies target package manifests and ordered
  migration ledgers, copies verified component/wiring facts, re-gates wirings
  and flips pointers. Failure is resumable; no deployment saga is introduced.
- Registration identity is immutable release content. Hot operational state uses
  pointer-flipped activation; it does not mutate a manifest digest.
- OTel carries trace context, one span per component invocation/effect, and
  throughput and error metrics for each wiring and registration.
  Retained broker advisories identify exhausted and terminated deliveries.
  The studio live view remains a bounded, redacted stream from router edges.
- Platform artifacts retain the operator/OCI path, converged by operator kubectl
  per deploy/README.md's runbook with a GitOps controller demand-gated.
  Flow-language artifacts and plan-shaped publication surfaces retire with their
  subjects.

## Proof and delivery

- All package, WIT, wire and schema versions remain `0.1` through MVP.
- The release-manifest integer format and OCI media type remain `v1` while the project is greenfield.
  Changes update the single current format without version bumps or compatibility machinery.
- The gate registry is exhaustive for living gates. Every entry resolves to a
  live manifest or recipe; retired surfaces keep no corpse coverage. Commands,
  artifacts and dependencies derive from those sources, not duplicated registry
  fields. D-number metadata is historical provenance only.
- A live gate that did not execute is not green. Environment-gated proofs expose
  skips, independently report every leg and use disposable state where roles or
  cluster-wide authority are involved.
- Promotion to `main` follows only after the entire `wamn-0h0g` scope-reduction
  program is resolved and the final RC is green on the resulting tip; the
  displaced pre-pivot tip remains archived at `archive/mvp`.

### Retained roots

Transitional packages remain only while they serve one of these named outcomes;
retirement beads remove the package and its marker together.

| Outcome |
|---|
| crash floor · M0 execution · flow composition |
| M0 authenticated admission via the warm run-worker |
| event spine (causation depth = loop guard) |
| wake-from-zero |
| publish gate |
| provisioning · publish · additive schema · tenant isolation (T1 minting) |
| management auth |
| runtime-checked sqlx over the credential-hiding `wamn:postgres` transport |
| egress confinement (import allowlist, mutation-proofed) |
| M0 node set |
| proof floor |

## Owned tradeoffs

- Component supply-chain checks return as a load-bearing boundary.
- At-least-once permits duplicate effects; authors provide idempotency and the
  platform deduplicates only at named admission boundaries. Node retry being off
  does not mean there are no duplicates, because a redelivery or an expired
  lease replays work the platform already sent.
- Per-edge host crossings buy shared pools and observability; WAC is the escape.
- The default tier trades durable node history for traces and a bounded live view.
- Hot wiring is accepted only with typed shape checks, semantic gates, versioned
  activation and instant rollback.
