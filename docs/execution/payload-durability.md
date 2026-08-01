---
status: implementation-period design
genre: design
date: 2026-08-01
owner: PLAN item 1
authority: docs/PLAN/PLAN.md
delete-when: PLAN item 1 closes after durable decisions are folded into PLAN.md
---

# Payload durability and the blob boundary

This document fixes the implementation shape for PLAN item 1. The plan remains
the decision authority; this file exists only while the checkpoint, offload, and
streaming work is being implemented.

## Scope and invariants

Item 1 replaces capture-replay recovery with a boundary-checkpoint reader,
makes the three durable storage classes independent, and supplies the platform
payload handle used by later bulk nodes. It does not change run lifecycle
transitions, build the client-facing blob node, or choose its authoring UX.

The implementation preserves these invariants:

1. Capture mode cannot change whether a run can recover.
2. A successful attempt and the checkpoint containing its successor frontier
   become visible in one PostgreSQL transaction.
3. Blob bytes become durable before a durable reference to them is committed.
4. Active checkpoints, replay/audit seeds, and captures have separate schemas
   and retention decisions.
5. Platform payloads and client objects have separate namespaces, credentials,
   authorization, accounting, and garbage collectors.
6. The resolved node contract is the only recovery classifier. Runtime node
   type tables and environment configuration cannot override it.
7. A retained replay seed makes referenced immutable material retainable, not
   necessarily executable. Current authorization still governs live execution.

## One recovery reader

The recovery entry point reads a versioned checkpoint from `runs.state_json`
and restores `ExecutionState` directly. It does not select completed capture
rows and does not call the current `reconstruct()` fold over
`node_runs.output_json`.

`RecoveryCheckpointV1` has this logical schema:

| Field | Meaning |
|---|---|
| `version` | Exact checkpoint encoding version; initially `1`. |
| `frontier` | Ordered pending tokens, each containing node id and a payload value; occurrence is derived when the token is promoted from this order plus `visits`. |
| `current` | Absent at a completed boundary; present only for a durably parked retry/wait with node id, payload, attempt, engine deadline, and throttle key; occurrence remains derived from `visits`. |
| `visits` | Completed occurrence count by node id. |
| `step-seq` | Next monotonic execution sequence. |
| `context` | Faithful, unscrubbed run context. |
| `result` | Last reducer result, including a response already released before downstream work continues. |
| `caller` | Caller-release state needed to preserve the one-result CAS semantics. |

A checkpoint payload value is tagged `inline` or `ref`. An inline value contains
canonical JSON. A reference contains an opaque run-scoped handle, content
digest, byte length, framing, and codec. Blob location, bucket, credentials, and
endpoint never enter the checkpoint.

The checkpoint codec belongs beside the pure execution engine and exposes only
`snapshot(&ExecutionState)` and `restore(plan, run_id, checkpoint)`. Its tests
round-trip every state member; the fields remain private to the engine so a
second representation cannot drift into the driver. Unknown checkpoint
versions fail as an infrastructure error and preserve the row for diagnosis.
There is no fallback to captures.

Fresh admission writes the entry frontier as checkpoint version 1. Each
successful or error-routed completion computes the successor state first, then
uses the existing fenced transition to commit the attempt result and replace
the whole checkpoint atomically. Parking commits the same checkpoint shape with
`current` populated. Terminalization removes the active checkpoint only after
the terminal outcome and replay seed are durable in that same transaction.
The run-state adapter converts the engine's invocation-relative retry deadline
to and from the durable queue clock; a process restart must preserve the
remaining delay rather than treating every reclaimed wait as immediately due.

`node_runs` remains authoritative for effect-attempt facts: recovery class,
attempt number, attempt key, dispatch/commit timestamps, and the explicit
outcome. On reclaim, the checkpoint identifies the outstanding occurrence and
the attempt row decides what the resolved recovery class permits:

| Resolved recovery class | Interrupted attempt result |
|---|---|
| `replay` | Re-dispatch from the checkpoint payload. |
| `idempotent-with-key` | Re-dispatch with the persisted attempt key. |
| `never-replay` | Produce the explicit `effect-uncertain` outcome. |

No completed-node history is folded to find the frontier. This also leaves
fan-out addable: an ordered frontier can contain several tokens, and all tokens
created by one emission are checkpointed together before any branch advances.

## The three durable classes

The SQL migration makes ownership visible instead of retaining one overloaded
history model.

### 1. Active recovery checkpoint

`runs.state_json` contains only `RecoveryCheckpointV1`. It is updated as one
document because the frontier is one consistency unit; referenced bytes may be
offloaded. It scales with the nonterminal frontier, not completed node count,
and is cleared at terminalization.

The run row pins the artifact hash, execution-bundle identity, and effective
payload-policy version used to interpret the checkpoint. Those pins are not
copied into every token.

### 2. Retained replay/audit seed

The immutable seed consists of the admitted input (inline or reference), pinned
artifact hash, pinned execution-bundle identity, invocation and admission
context, platform revision, run lineage, caller outcome, and `node_runs` effect
attempt facts. Partial rerun also needs the faithful input of its chosen
occurrence; that value is part of the seed, not capture.

The run input and occurrence inputs therefore use paired `*_json` / `*_ref`
columns with an exactly-one-or-null constraint. The referenced artifact stores
the full canonical `ResolvedNodeContract` set, so both standard and custom
nodes recover under the same pinned recovery, interface, capability,
connection-requirement, and executable identities.

Seed retention is policy-governed after terminalization. Deleting a seed first
deletes its authoritative payload-reference edges; only then can referenced
blob collection make the bytes eligible. Retention of a bundle, payload, or
connection definition does not bypass revocation or current authorization for
live re-execution.

### 3. Observability capture

`node_captures` is an optional one-to-one child of a node occurrence. It owns
input/output preview, size, digest, effective capture mode, redaction marker,
and optional bounded inline captured values. `full`, `scrubbed`, `preview`, and
`off` affect only this table; a value above `capture.max-bytes` is preview-only,
not another reference root. Capture retention may be shorter than replay-seed
retention, and deleting it cannot alter a checkpoint, seed, attempt, or caller
outcome.

During the migration the existing `node_runs` capture columns may be copied
before they are removed, but the checkpoint reader must stop consuming them in
the first compatibility-breaking schema revision. This repository is pre-version
alpha, so from-zero provisioning is the migration gate; no dual recovery path
is retained.

## Platform payload objects

Platform payload bytes use a project-environment-scoped content address. The
opaque handle resolves only with the owning tenant, project, environment, and
run claims. A digest is never accepted as authority and is never exposed as a
cross-project existence probe.

The metadata store records immutable object identity, digest, length, framing,
codec, creation time, finalized time, and namespace. A separate reference-edge
table records an owner class (`checkpoint`, `replay-seed`, or `caller-outcome`)
and owner identity. Capture is deliberately absent from this set: its bounded
inline value or preview cannot extend platform-object retention. All metadata
tables use the same forced tenant RLS floor as run state.

Physical deduplication is allowed only below this logical boundary. It must not
couple authorization, reachability, encryption keys, retention, billing, or an
upload response across project environments.

### Blob-before-reference protocol

Every inline offload and streaming write follows one order:

1. Create a platform-namespace staging object scoped to the run.
2. Stream bytes while counting uncompressed size and enforcing the ceiling.
3. Finish the stream, compute and verify its digest, then atomically finalize
   the immutable content-addressed object.
4. Confirm the finalized object is readable.
5. In the fenced PostgreSQL transaction, insert the reference edge and commit
   the attempt completion plus successor checkpoint.

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
the attempt completion and successor checkpoint do not partially commit.

### Two garbage collectors

Referenced-object collection is a reachability sweep, not a duration rule. Its
roots are nonterminal checkpoints, retained replay/audit seeds, and retained
caller outcomes. A candidate is deleted only if the metadata row still has no
reference edge in the deleting transaction. The sweep is scoped to the
platform namespace and project environment.

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

The implementation candidate uses the existing `wamn:node@0.1.x`
`streamed(payload-ref)` and `payloads` import over
`wasi:io/streams@0.2.6`. This would keep the frozen P2 ABI and avoid waiting for
component-model async. The handle is transport-neutral, so a later P3 adapter
could relocate a stream handle internally without changing payload identity.
The candidate does not become durable direction until decision bead
`wamn-4u7p.3` compares it with a P3-native breaking ABI and folds the result
back into `PLAN.md`.

The canonical resolved-node contract shipped at `95eb37a` remains the identity
boundary. Its contract version, exact WIT interface identity, declared ports,
capability classes, portable connection requirements, recovery class, and
executable identity are consumed by artifact validation, replay
classification, and execution-bundle identity. Item 1 must not add bucket,
endpoint, threshold, ceiling, credential, or environment attestation to it.
Nodes importing `payloads` resolve to the corresponding strict WIT world; the
host-side transparent offload path does not change a node's contract.

## Configuration and threshold decision

The environment owns these settings:

| Setting | Meaning |
|---|---|
| `payload.inline-threshold` | Compressed stored size above which an inline value is transparently offloaded. Candidate starting point: 256 KiB; not a default until measured. |
| `payload.ceiling` | Maximum uncompressed logical payload accepted by admission or one emission/stream. No default until measured. |
| `payload.compress` | Codec policy for inline checkpoints and platform objects; enabled in the characterization matrix. |
| `payload.store` | Platform object-store instance and namespace; never serialized into an artifact or checkpoint. |
| `payload.orphan-ttl` | Minimum age before the orphan collector may act; set above the measured worst-case upload-plus-commit interval. |

Configuration validation requires a positive threshold below the ceiling and a
known codec. Each run pins a policy version plus the effective numeric threshold
and ceiling in its replay seed for diagnosis. Placement is not semantic: a
retained payload remains readable after settings change, and threshold changes
do not alter artifact or execution-bundle identity.

The production default is selected from the sustained sweep, not copied from an
industry example. The hard ceiling must also fit the ingress limit, bounded
stream counter, object-store multipart constraints, and the configured guest
memory ceiling. Per-flow policy may lower the environment ceiling but never
raise it.

## Proof design

### Deterministic correctness gates

- With capture `off`, kill after several boundaries, reclaim on another
  executor, and compare the terminal outcome and effect-attempt ledger with a
  capture `full` control.
- Kill immediately before and after attempt completion/checkpoint commit for
  each resolved recovery class. Assert replay, stable-key replay, or explicit
  `effect-uncertain` exactly as pinned in `ResolvedNodeContract`.
- Kill at every blob protocol boundary. Assert either no durable reference or a
  readable object, then resume the run from the previous or new checkpoint as
  appropriate.
- Store the same content across five boundaries and across two project
  environments. Assert one logical object per environment, idempotent rewrites,
  no cross-environment enumeration, and no cross-environment GC.
- Retain a replay seed after terminal checkpoint deletion and prove its payload
  remains readable; delete the seed and prove it becomes collectible. Prove the
  orphan collector does not delete a young pre-reference object.
- Run scrubbed capture while asserting the checkpoint and replay seed retain the
  faithful value. No test or diagnostic prints that value.
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
`docs/results/ceilings.md`.

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

## Exit evidence

Item 1 can close only when the deterministic gates pass from a clean database,
the sustained sweep and raw data are published with reproducible commands, the
before/after F1 comparison matches the predicted storage-class scaling, and
the configured threshold and ceiling cite that evidence. At closure, durable
decisions return to `PLAN.md`, subsystem docs describe the shipped code, and
this implementation-period document is deleted.
