---
status: item-local
plan-item: 2A
bead: wamn-ayq7.5
date: 2026-08-01
---

# Execution-bundle specialization experiment

This document fixes the experiment that chooses packaging granularity for
[PLAN item 2A](../PLAN/PLAN.md#2a--composition--capability-by-construction). It is
implementation-period detail: durable conclusions fold back into `PLAN.md`, and this file
is removed when item 2A closes.

## Question and hypotheses

Both candidate shapes preserve the plan as data and use a pinned WAC linker. They differ
only in the executable plug boundary:

| Arm | Plug contents | Structural consequence |
|---|---|---|
| **N — exact node** | one node implementation per plug | only selected implementations enter a bundle; each node is its own fault, revocation, patch, and release unit |
| **C — capability class** | all first-party implementations in one `pure`, `http`, or `postgres` plug | selected classes enter a bundle; unused implementations in a selected class share memory and release fate |

The primary hypothesis is that C makes a fleet-wide platform upgrade materially easier to
stage because it produces fewer distinct plugs and bundles. The counter-hypothesis is that
observed usage bounds N tightly enough that it meets the same operational budgets while
retaining the stronger isolation boundary. Neither cache cardinality nor single-bundle link
time decides the question alone.

A linked, uncomposed runner is measured as the fallback comparator, not as a third
packaging candidate. It remains the outcome if composition itself fails the hard gates.

The experiment replays one frozen, representative fleet manifest through both arms. The
manifest contains the observed first-party node sets and invocation weights, including at
least one pure-only flow and repeated node sets across flows, projects, and organizations.
N and C receive identical runner bytes, node implementations, adapters, traffic, resource
limits, registry, and cache capacity.

## Bundle identity and reproducibility

The shipped canonical `ResolvedNodeContract` is the only node-resolution input. It carries
the resolved-contract version, exact interface/WIT identity, ports, capability classes,
portable connection requirement identities, recovery contract, and executable identity.
Changing any of those fields must invalidate both the artifact and the execution-bundle
key, as established by the canonical-resolution mutation proof.

For this experiment, an execution-bundle key commits to every input that can change the
composed bytes:

```
identity format version
packaging arm and deterministic plug layout
runner/platform revision and digest
ordered canonical resolved-node contracts
ordered plug manifests and component digests
ordered adapter identities and digests
pinned composition-tool identity
```

The existing `ExecutionBundleIdentity` supplies runner revision, ordered canonical
resolutions, and adapters. The experiment must extend or frame that identity with the
remaining inputs; it must not maintain a second node descriptor. The builder records the
key, all input digests, tool identity, output digest, and composition log as provenance, and
rebuilding the same key must reproduce the same output digest. A mismatch poisons the
cache entry and fails the gate.

In arm C, the plug manifest and digest cover every implementation carried by a selected
class, including implementations unused by the flow. Updating any member therefore changes
every bundle containing that class. In arm N, only selected implementations participate.

Environment connection instances, bindings, credentials, attestations, and generations
never enter this identity. The portable connection requirement type and contract identity
already do, through the canonical resolved-node contract.

## Cache and entitlement

Composition is idempotent and single-flight by execution-bundle key. The experiment uses a
shared first-party cache across environments, projects, and organizations, but a cache hit
is only eligible after the requester is authorized to read every input plug and the output
bundle. The cache index may be global; authorization is evaluated on every lookup and
delivery. Revocation prevents new delivery immediately and does not turn possession of a
digest into entitlement.

Only platform-designated first-party inputs are eligible for global reuse in 2A. Private or
custom components, entitlement-scoped reuse, and their economics belong to item 2D and are
excluded from the hit-rate result. Metrics report raw content matches separately from
authorized hits so an authorization miss cannot be counted as reuse.

## Capability-bearing tranche

The pure-only fleet slice can run immediately. The `http` slice is admissible
only after item 2B supplies its minimum typed-connection WIT and host adapter. Its node must
declare the portable connection requirement in its canonical resolution and receive the
resolved connection through the typed host capability. A node importing generic HTTP and
constructing an absolute URL is an invalid fixture, not partial evidence.

The capability-bearing proof inspects the final component imports. They must contain only
the selected capability worlds and connection adapter; filesystem, sockets, messaging,
blobstore, and unselected capability classes must be absent. The same portable artifact is
run against two environment connection bindings without changing artifact or bundle
identity. Any material change to the 2B WIT or adapter identity invalidates and reruns this
tranche and the final economics.

Postgres is not an admissible fixture for this experiment. Item 2B deliberately confines its
provider prototype to a low-risk HTTP target.

## Platform-upgrade drill

For both arms, build the observed fleet at platform revision R0, start a mix of short,
queued, running, and parked runs, then introduce R1. The drill must:

1. enumerate the R1 bundle set from the same fleet manifest and compose it with single-flight
   deduplication under concurrent requests;
2. verify and publish every R1 bundle before it is eligible for routing;
3. create at least one clean, invocation-ready instance for each bundle with routed demand;
4. atomically move new admissions to R1 only after that readiness barrier;
5. keep every R0 bundle fetchable while a queued, running, parked, recovery, or retained
   replay/audit record refers to it; and
6. retire R0 instances and delete R0 registry objects only after the final reference and
   retention obligation are gone.

This is the no-cold-start-window invariant: the routing pointer never names an unavailable
or wholly cold bundle. It does not claim that every scale-out invocation is warm. A failed
R1 build or warm-up leaves routing on R0, and rollback reuses the retained R0 objects.

Measure per arm: distinct plugs and bundles; cache requests, raw matches, authorized hits,
misses, and single-flight collapses; compose queue time and p50/p95/p99 link latency;
end-to-end fleet rebuild and readiness time; bytes read and written; registry object count,
stored bytes, request rate, and peak concurrent uploads; warm and cold invocation latency;
RSS and Wasmtime allocation footprint per ready instance; instances and concurrent runs per
executor; and any unavailable-bundle or cold-routing event. Report totals and distributions,
not one aggregate hit rate.

## Pool hygiene and retirement

Density comes from a bounded pool of wamn-owned execution instances. Each instance owns one
store and `ExecutionState`, serves one invocation at a time, and is returned to its pool only
after invocation-scoped state is cleared. Upstream linked-call or P3 HTTP `pool_size` is not
accepted as evidence that this pool exists.

The hygiene gate alternates organizations, projects, runs, connection bindings and
generations, credentials, egress grants, trace contexts, configs, and sentinel guest-memory
values across reused instances. The next invocation must observe none of the prior values.
Identical component bytes and a correct bundle key do not satisfy this test.

An instance is destroyed, never repooled, after a trap, cancellation, deadline interruption,
failed cleanup, revision or entitlement invalidation, or its configured maximum invocation
count. Clean idle instances may be retired to meet the per-bundle idle limit, global memory
budget, or idle-age limit. The pool stops admitting work before retirement and never shares
one store between concurrent runs. Bundle-object retention is independent: destroying the
last warm instance cannot delete bytes still referenced by durable work.

## Decision rule and falsifiers

Before measurement, the owning implementation beads record the existing rollout,
availability, registry, latency, and executor-memory budgets used by the gate; the experiment
does not tune budgets after seeing an arm's result.

Both arms must first pass the hard invariants: deterministic identity, exact declared import
ceiling, typed-connection tranche, authorized cache delivery, no unavailable or wholly cold
routing during the upgrade, old-bundle retention, and zero cross-invocation leakage. An arm
that fails one is ineligible regardless of economics.

- Choose **C** only if N breaches a pre-recorded fleet recomposition, registry-pressure, or
  ready-instance density budget and C passes every hard invariant and budget.
- Otherwise choose **N** when it passes, because it retains the narrower fault, patch,
  revocation, and upgrade boundary.
- If neither composed arm passes, keep the linked runner and the explicit bundle identity,
  strict node contracts, common draft/published path, environment-scoped connections, and
  code-enforced per-node capability narrowing. Only the structural outer capability ceiling
  is deferred.

The result is falsified, and must not close 2A, if an identity-affecting mutation aliases an
old key; identical keys produce different bytes; an unauthorized caller receives a cached
bundle; an unused capability appears in final imports; the capability fixture bypasses 2B;
an upgrade routes to absent or wholly cold bytes; a referenced old bundle is collected; a
sentinel crosses invocations; measured pressure exceeds its recorded budget; or the fleet
manifest omits observed high-cardinality node sets.

## Gate of record

The named gate is **`execution-bundle-specialization`**, exposed as a `wamn-gates
bundlebench` command and a two-stage in-cluster Job. It emits a versioned JSON verdict with
the git revision, runner/platform and composition-tool identities, fleet-manifest digest,
both arm identities and metrics, 2B WIT/adapter identity, every hard-invariant result, the
pre-recorded budgets, and the selected outcome.

The gate owns three proof layers:

- deterministic unit/property tests for key framing, ordering, digest sensitivity,
  single-flight behavior, authorization, reference accounting, and retirement;
- component inspection plus deliberate mutations that remove an identity field, add an
  undeclared import, bypass entitlement, prematurely collect R0, or skip instance reset;
- the in-cluster R0-to-R1 fleet drill with concurrent composition, registry traffic, warm
  pools, mixed durable references, and the capability-bearing flow.

Every deliberate mutation must make the named gate red. A local benchmark or a pure-only
run is diagnostic evidence, not the gate-of-record verdict.
