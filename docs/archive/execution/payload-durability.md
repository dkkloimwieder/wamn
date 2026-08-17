---
status: superseded historical design
genre: design record
date: 2026-08-01
owner: PLAN item 1
authority: docs/archive/PLAN/PLAN.md
---

# Payload durability and the blob boundary

> **Withdrawn execution design.** This file records the payload/checkpoint
> proposal evaluated during PLAN item 1. It is not an active implementation
> contract. `wamn-0h0g.4.5` removed the reconstruction, restore, replay, and
> partial-rerun surfaces; `wamn-0h0g.4.9` made the immutable effect-attempt
> ledger the sole crash-classification authority.

## Current execution boundary

The flow engine performs one in-memory walk. It has no durable frontier codec,
snapshot/restore entry point, completed-history fold, or mid-graph seed. A
`runs.state_json` value is not a recovery checkpoint and capture rows do not
authorize execution.

When an expired claim has no immutable effect attempt, the pre-effect claim
path may clear replaceable projections and restart the single-shot walk from
zero. If any immutable effect attempt exists, the run is fenced and becomes
`effect-uncertain`; it is never redispatched. Capture mode neither permits nor
prevents that classification.

There is no retained execution-lineage schema. The former `replay_of`,
`root_run_id`, `runs_root`, `RecoveryCheckpointV1`, engine `snapshot`/`restore`,
and arbitrary rerun seed are historical names only.

## Historical proposal and ruling

The withdrawn proposal tried to make capture-independent continuation possible
by serializing the complete engine frontier, visit counters, active retry,
context, result, and caller state into a versioned document. It also proposed
three separately retained classes: an active frontier checkpoint, a faithful
rerun/audit seed, and scrubbed observability capture.

That shape was not adopted. It created a second execution-state representation,
made crash behavior depend on a restoration codec, and permitted effect
redispatch classes that conflict with the conservative attempt ledger. The
replacement rule is intentionally smaller: restart only before any immutable
effect attempt; otherwise stop as `effect-uncertain`.

Several conclusions from the investigation remain valid:

1. Capture is observability, never effect authority.
2. Blob bytes must become durable before a database reference to them commits.
3. Backend location, bucket, endpoint, and credentials must not enter portable
   artifact or execution-bundle identity.
4. Platform payload objects and client-owned objects require separate
   namespaces, credentials, authorization, accounting, and collection.
5. A digest identifies content; it does not grant cross-project access or
   provide an existence oracle.

References below to the withdrawn checkpoint/replay model are preserved only
where they explain the historical blob proposal or its former proof plan. They
are not current acceptance criteria.

## Deferred platform payload-object proposal

This section preserves the blob/streaming design for future work. It does not
create an execution checkpoint or rerun surface.

Platform payload bytes use a project-environment-scoped content address. The
opaque handle resolves only with the owning tenant, project, environment, and
run claims. A digest is never accepted as authority and is never exposed as a
cross-project existence probe.

The metadata store would record immutable object identity, digest, length,
framing, codec, creation time, finalized time, and namespace. A separate
reference-edge table would record a durable owner class and owner identity; the
exact owner set must be chosen by the future design rather than inherited from
the withdrawn checkpoint/rerun model. Capture is deliberately absent: its bounded
inline value or preview cannot extend platform-object retention. All metadata
tables use the same forced tenant RLS floor as run state.

Physical deduplication is allowed only below this logical boundary. It must not
couple authorization, reachability, encryption keys, retention, billing, or an
upload response across project environments.

### Blob-before-reference protocol

A future inline offload or streaming write follows one order:

1. Create a platform-namespace staging object scoped to the run.
2. Stream bytes while counting uncompressed size and enforcing the ceiling.
3. Finish the stream, compute and verify its digest, then atomically finalize
   the immutable content-addressed object.
4. Confirm the finalized object is readable.
5. In the owning fenced PostgreSQL transaction, insert the reference edge and
   commit the corresponding durable transition.

A crash before step 3 leaves only an expired staging upload. A crash after step
3 but before step 5 leaves an uncommitted orphan. Retrying steps 1–4 is
idempotent. A database row must never name a staging object, and step 5 refuses
a handle whose finalized metadata is absent. Consequently no crash point can
create a dangling durable reference.

An admitted request follows the same order before the run row and queue row are
created. If it exceeds the ceiling, admission returns the typed
`payload-limit-exceeded { observed-bytes, ceiling-bytes }` rejection and commits
neither a run nor a reference. For a node emission, the same typed condition is
routed through the node's catchable error path; the existing run remains, but
the attempt completion and payload reference do not partially commit.

### Two garbage collectors

Referenced-object collection is a reachability sweep, not a duration rule. The
future design must name its actual durable owner roots. A candidate is deleted
only if the metadata row still has no reference edge in the deleting
transaction. The sweep is scoped to the platform namespace and project
environment.

Uncommitted-orphan collection is a separate age sweep. It considers finalized
platform objects and abandoned staging uploads that have no reference edge and
whose creation/finalization predates `payload.orphan-ttl`. The cutoff prevents a
collector racing the blob-before-reference window. Its conditional delete
rechecks both age and absence of references.

Neither collector possesses client-blob credentials or addresses a client
bucket. Namespace-isolation tests use identical digests on both sides so a
mistaken content-address-only delete cannot pass unnoticed.

## Platform offload and client streaming remain distinct

Platform offload is an invisible host action after a node returns an inline
value. It adds no node capability and protects PostgreSQL from medium payloads,
but it does not reduce the guest memory already used to construct that value.

Client-directed bulk transfer is an explicit, capability-bearing node owned by
item 2C. It uses a bounded stream so the complete business object never enters
guest linear memory. Client objects have client-selected lifetime and can be
used by other flows and product surfaces; platform objects remain run-scoped
implementation details.

Decision `wamn-4u7p.3` activates the existing `wamn:node@0.1.x`
`streamed(payload-ref)` and optional P2 `payloads` import. The durable value
crossing node boundaries is the opaque `payload-ref`, not a live stream
resource. Passing it moves no bytes. A bulk-capable endpoint opens the reference
with `payloads.read`, or creates one with `payloads.create`, and transfers bytes
through bounded, backpressured `wasi:io/streams` resources. The complete object
never enters guest linear memory and component-model async is not required.

This choice is grounded in the exact runtime, not only API preference. The
pinned `wash-runtime` v2.7.0 fork at `daba6029` supports parallel P2 and P3 host
surfaces. Its P3 cross-store relocation extracts `stream<T>` into a live
no-buffering channel pump, keeps the producing store alive while results drain,
and bounds that drain with a timeout. P3 can therefore bridge a logical stream
handle between component stores, but host code still pumps elements; it is not
zero-copy relocation of the platform object's storage handle. WAMN already has
that storage handle in `payload-ref`, so a breaking P3-native node ABI buys no
item-1 invariant.

Before the currently inert import is enabled, its provisional `wasi:io` version
pin is aligned across the authoritative WIT, every generated dependency copy,
and the pinned host. The aligned strict interface identity enters the canonical
resolved-node contract. The host implementation resolves a handle only under
the active run/project/environment, reads or finalizes directly against the
platform store, enforces logical-byte ceilings, and never exposes backend
location or credentials. A returned create handle remains ineligible for an
attempt-owned durable reference until its output stream is closed, finalized, and
verified by the blob-before-reference protocol.

`wamn:node@0.2` remains the later coordinated WASI 0.3/native-async revision
owned by `wamn-72i`. It is not an internal adapter hidden behind 0.1: changing
the handler's stream shape changes the strict interface identity and requires a
deliberate SDK, component, builder-policy, composition, and migration campaign.
Revisit only after the target ABI and P3 service lifecycle are stable and a
measured cross-component stream case justifies that cost.

The canonical resolved-node contract shipped at `95eb37a` remains the
executable identity boundary. Its contract version, exact WIT interface
identity, declared ports, capability classes, portable connection
requirements, executable recovery contract, and executable identity are
consumed by artifact validation and execution-bundle identity. The current
immutable effect-attempt ledger, not a payload handle or rerun classifier,
owns crash authority. Item 1 must not add bucket, endpoint, threshold, ceiling,
credential, or environment attestation to either layer.
Nodes importing `payloads` resolve to the corresponding strict WIT world; the
host-side transparent offload path does not change a node's contract.

## Deferred configuration and threshold decision

The historical proposal named these future environment settings; none receives
a production default here:

| Setting | Meaning |
|---|---|
| `payload.inline-threshold` | Compressed stored size above which an inline value is transparently offloaded. Candidate starting point: 256 KiB; not a default until measured. |
| `payload.ceiling` | Maximum uncompressed logical payload accepted by admission or one emission/stream. No default until measured. |
| `payload.compress` | Codec policy for inline values and platform objects; enabled in the characterization matrix. |
| `payload.store` | Platform object-store instance and namespace; never serialized into an artifact or execution identity. |
| `payload.orphan-ttl` | Minimum age before the orphan collector may act; set above the measured worst-case upload-plus-commit interval. |

Configuration validation requires a positive threshold below the ceiling and a
known codec. A future durable diagnosis record would pin the effective policy
version, threshold, and ceiling. Placement is not semantic: a
retained payload remains readable after settings change, and threshold changes
do not alter artifact or execution-bundle identity.

The production default is selected from the sustained sweep, not copied from an
industry example. The hard ceiling must also fit the ingress limit, bounded
stream counter, object-store multipart constraints, and the configured guest
memory ceiling. Per-flow policy may lower the environment ceiling but never
raise it.

## Historical proof design (withdrawn)

> Everything in this section is preserved as proposal provenance. These are not
> current execution or acceptance gates, and the checkpoint/rerun cases must not
> be reintroduced.

### Deterministic correctness gates

- With capture `off`, kill after several boundaries, reclaim on another
  executor, and compare the terminal outcome and effect-attempt ledger with a
  capture `full` control.
- Kill immediately before and after attempt completion/checkpoint commit for
  each resolved recovery class. Assert replay, stable-key replay, or explicit
  `effect-uncertain` exactly as pinned in the artifact occurrence binding and
  admitted in the attempt fact.
- Kill at every blob protocol boundary. Assert either no durable reference or a
  readable object, then resume the run from the previous or new checkpoint as
  appropriate.
- Store the same content across five boundaries and across two project
  environments. Assert one logical object per environment, idempotent rewrites,
  no cross-environment enumeration, and no cross-environment GC.
- Retain a replay seed after terminal checkpoint deletion and prove its payload
  remains readable; delete the seed and prove it becomes collectible. Prove the
  orphan collector does not delete a young pre-reference object.
- Run `full` capture with a known-pattern secret while asserting the author projection is
  scrub-redacted and the checkpoint/replay seed retains the faithful value. No test or
  diagnostic prints that value.
- Submit one byte over the ceiling and assert a typed rejection, no run, no
  queue row, no reference edge, and no finalized object beyond an eligible
  orphan. Exercise the analogous node-emission error path.
- Round-trip checkpoints containing a loop occurrence, parked retry, error
  route, caller-release state, and a multi-token frontier. Reject unknown
  versions without consulting capture.

### Sustained threshold sweep

Run the canonical F1 path before and after at logarithmic payload sizes from
1 KiB through 10 MiB, bracketing the candidate crossover densely. Test several
concurrencies against both the in-cluster store and an external-compatible
store, with compression on and off. Each level has a warm-up, a sustained
steady interval long enough to observe at least two autovacuum opportunities,
and a drain; a single-request or burst-only result is inadmissible.

For every level publish raw rows with git revision, image digest, PostgreSQL and
object-store durability settings, payload distribution, concurrency, duration,
and effective policy. Measure boundary p50/p95/p99, completed runs/s, WAL bytes
per run, heap/index/TOAST growth, dead tuples, autovacuum count and duration,
object PUT/GET latency and bytes, checkpoint bytes, blob/reference counts,
orphan age/reclaim latency, executor RSS, and errors. Run enough completed runs
to compare directly with the existing PLAN-3-F1 capture-on baseline in
`docs/archive/results/ceilings.md`.

Choose the inline threshold at the lowest sustained crossover where offload
materially reduces WAL and database growth without an unacceptable boundary
latency penalty. Choose the ceiling at the lowest independently observed safety
limit across ingress, guest memory, streaming, and store behavior, with margin;
do not infer it from the threshold. Publish curves and the decision rule, not
only the selected numbers. Item 3 consumes this architecture and evidence for
the R6 verdict.

### Mutation obligations

The gate is not accepted until each deliberate mutant makes a named test red:

| Mutant | Test that must fail |
|---|---|
| Recovery reads `node_runs.output_json` or rejects capture `off` | capture-independent reclaim |
| Attempt success commits without the successor checkpoint | atomic completion crash-window test |
| Reference commits before blob finalization/readability | blob crash-matrix test |
| Replay-seed roots are omitted from reachability | retained-seed GC test |
| Orphan age/recheck is removed | young pre-reference object race test |
| Namespace scope is removed from lookup or deletion | equal-digest cross-environment isolation test |
| Recovery class comes from node type instead of the resolved contract | standard/custom recovery-identity mutation test |
| Threshold campaign accepts a burst-only sample or omits WAL | sustained-sweep result validator |

This is the execution-model rigor required by PLAN risk 6: model/state-machine
tests, explicit crash points, in-cluster durable-store gates, and mutation
proofs for the load-bearing claims. It does not require repeated prose-review
rounds, and it does not impose this proof burden on later authoring UI work.

## Historical exit evidence (withdrawn)

The proposal said item 1 could close only when the deterministic gates passed
from a clean database,
the sustained sweep and raw data are published with reproducible commands, the
before/after F1 comparison matches the predicted storage-class scaling, and
the configured threshold and ceiling cite that evidence. At closure, durable
decisions return to `PLAN.md`, subsystem docs describe the shipped code, and
this implementation-period document is deleted.
