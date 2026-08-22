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
3. **Deployment.** Platform artifacts use OCI, GitOps and operator-managed hosts.
   Wirings are gated tenant rows activated by pointer flip, not CRDs or per-edit
   artifacts.

## Runtime target

- Retire the flow language, expression configs, standard nodes as built-ins, the
  flowrunner guest, execution plans/compiler, frames/call-flow, per-node durable
  facts and capture.
- Rehost the proven frontier walk, port routing and error-edge semantics in one
  host-native router shared by HTTP and queued execution. A delivery resolves an
  active wiring, acquires a component instance per node, invokes its operation,
  routes outputs and ends in `respond`, `emit` or discard under a hop limit.
- Key wiring resolution by `(tenant, catalog, environment)`; the tenant identity
  is mandatory even when environment names match.
- Use per-component-digest pools across wirings. The Wasmtime pooling allocator
  is already enabled, but the repository's instance pool is still unwired and
  its 512-slot limit is hard-coded. Fresh instances remain the rule; memory reuse
  waits for explicit affinity/windowed-state semantics. Pool sizing becomes a
  measured deployment value.
- Keep WAC composition only as a demand-gated fusion optimization for measured
  hot, pure pipelines; the default preserves shared pools and router-edge taps.

One `wamn-execution-host` driver serves HTTP and queued execution through the
uniform `wamn:node` seam and existing digest-keyed pool; services depend inward
on execution-host, runtime, catalog and router. `to_port` is enforced whenever a
target has multiple inputs and may be omitted only for a single-input target.

## Ingress and durability

1. **Hot HTTP:** attachment → router → response, with no run or queue row.
   Per-route in-flight bounds refuse excess work with 429.
2. **Streams:** per-registration durable pull consumers deliver one event or an
   ordered batch; ack follows completion, retry is bounded, and poison input goes
   to a capped per-registration DLQ.
3. **Automations:** admission atomically writes run and queue rows under a producer
   key; claim/lease hands work to the router; expiry redelivers. `begin`/`wait`
   and same-key/same-outcome remain.

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

## Data, identity and generated APIs

- `wamn:postgres` remains the credential-hiding database boundary. sqlx uses a
  custom runtime-checked `Database` over that transport; upstream `query_as!`
  supports only built-in drivers, so MVP makes no offline-cache or compile-time
  query-shape claim and does not widen WIT to imitate Postgres internals.
- Runtime database identities are per project-environment and tenant. PostgreSQL
  `current_user`, backed by opaque bounded role names, is the RLS input; caller-
  settable tenant GUCs retire. `wamn_app` becomes a NOLOGIN ACL role inherited by
  rotating login generations with no `SET ROLE` escape.
- Credential selection is host-owned and keyed by `(project, AuthorityClass)`.
  The closed classes are guest SQL, executor platform, callable HTTP and event
  materializer. HTTP and event admission are DB-enforced per-kind operations;
  producer kind is never trusted as a parameter.
- Generated APIs emit ordinary gated artifacts. `generate crud` produces wirings,
  route attachments and cases around one generic `entity` component. Nothing
  generated is gate-exempt or reflection-served.
- Existing compiled RLS policies use role/user claims that the production claim
  path does not inject. That correctness defect must close before generated APIs
  or raw developer SQL become reachable.

Per-tenant roles imply pools per credential. Connection multiplication is the
pooler trigger, not an alternative identity model.

## Release, promotion and observability

- A release closes over catalog version, component/interface digests, bindings
  and wirings. Promotion applies additive schema, pulls missing digests, verifies
  target bindings, re-gates wirings and flips pointers. Failure is resumable; no
  deployment saga is introduced.
- Registration identity is immutable release content. Hot operational state uses
  pointer-flipped activation; it does not mutate a manifest digest.
- OTel carries trace context, one span per component invocation/effect, and
  per-wiring/registration throughput, error, ack-lag and DLQ metrics. The studio
  live view is a bounded, redacted router-edge stream, not durable node history.
- Platform artifacts retain the operator/OCI/GitOps path. Flow-language artifacts
  and plan-shaped publication surfaces retire with their subjects.

## Proof and delivery

- All package, WIT, wire and schema versions remain `0.1` through MVP.
- The gate registry is exhaustive for living gates. Every entry resolves to a
  live manifest or recipe; retired surfaces keep no corpse coverage. Commands,
  artifacts and dependencies derive from those sources, not duplicated registry
  fields. D-number metadata is historical provenance only.
- A live gate that did not execute is not green. Environment-gated proofs expose
  skips, independently report every leg and use disposable state where roles or
  cluster-wide authority are involved.
- No merge to `mvp` occurs until the entire `wamn-0h0g` scope-reduction program is
  resolved and the final RC is green on the resulting tip.

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
| the Postgres standard node (`standard-nodes/src/postgres.rs`) |
| egress confinement (import allowlist, mutation-proofed) |
| M0 node set |
| proof floor |

## Owned tradeoffs

- Component supply-chain checks return as a load-bearing boundary.
- At-least-once permits duplicate effects; authors provide idempotency and the
  platform deduplicates only at named admission boundaries.
- Per-edge host crossings buy shared pools and observability; WAC is the escape.
- The default tier trades durable node history for traces and a bounded live view.
- Hot wiring is accepted only with typed shape checks, semantic gates, versioned
  activation and instant rollback.
