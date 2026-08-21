> **Archived 2026-08-21.** Superseded as current design by
> `docs/exe-model.md`; retained for decision provenance only.

# Deployment simplification — follow wasmCloud v2: operator-managed hosts, OCI artifacts, GitOps convergence

Status: RATIFIED (owner 2026-08-16, decision `wamn-0h0g.13.43` —
rulings 1–6 below; test-contract simplification ratified same day,
record `wamn-0h0g.13.44`) · supersedes the affected clauses of
`.2.4`, `.4.12`, `.8.19`, `.8.22`, `.9.9` (attestation scope), the
flow-execution amendment's release-bound-resolution section, and
parts of `.8.1`/`.8.2` (test contract) ·
owner-directed 2026-08-14, branch `mvp`, tracker `wamn-0h0g`,
implementation sub-epic `wamn-0h0g.15`.

## Ruling

wamn follows wasmCloud v2's deployment architecture wherever the
floor permits: the **runtime-operator** manages hosts and component
workloads as Kubernetes CRDs (state in etcd, reconciled over the NATS
we already run); everything immutable ships as **OCI artifacts**;
**GitOps converges** desired state. Runs are never version-pinned; a
run executes under the release its claiming pod carries and records
that fact once. The database keeps only tenant-runtime state: the run
plane, bindings/generations, applied schema, app data, and the
control-plane gate artifacts. Stated from the artifact side, the same
boundary reads: **the manifest carries release shape;
environment-operational state stays in Postgres** — the bindings
boundary applied again.

wamn's embedded `wash-runtime` custom host is the sanctioned v2
pattern ("substitute your own custom host builds") — adoption is
substitution, not redesign: our host images enter the operator
chart's `runtime.hostGroups[]` host tier.

## The model

**Publish (control plane, unchanged as the gate).** One transaction:
verify draft + green report → mint immutable release + evidence
(`tested_resolution_map` included). Then push the content-addressed
OCI artifacts — plan bytes and the **release manifest** (RFC 8785
bytes, `sha256:<digest>`; flow → flow-version / plan-hash /
source-artifact / binding-base-artifact / callable-contract, call-edge
adjacency, attachment + registration projections) — and write the
environment's desired
state to the GitOps source: the release-identity ConfigMap
`(release version, manifest digest)` and any changed CRDs. The
manifest is minted per `(release, environment)` and names that
environment in its hashed identity block, so one digest exists per
environment; under the boundary above every projected field is
release content, so the two documents for one release in two
environments differ by that label alone.

**Deploy (cluster, operator-reconciled).** Argo/Flux (plain
`kubectl apply` in dev) converges; the chart's `runtime.hostGroups[]`
run **our custom host images** (executor, flow-http, materializer
hosts) and the runtime-operator schedules component workloads onto
them; HTTP exposure rides operator-managed EndpointSlices on standard
Services (the deprecated gateway is never adopted). Pods surface the
release identity as immutable mounted config per the v2
ConfigMap/Secret pattern. Readiness stays ours and gates on:
manifest + referenced plans fetched and hash-verified, and every
connection requirement of the release **bound in this environment**.
Rollback is Git revert; wrong-target protection is
namespace/context targeting.

**Serve/admit.** flow-http and the materializer read routes and
registration identity from the mounted manifest; the two pieces of
environment-operational state stay behind. Enablement is checked
inside the admission transaction, which already hits the database,
and refuses through the existing `inactive-definition` classification
(`attachment-disabled` at the HTTP surface) — so emergency-off is one
`UPDATE`, effective on the next admission. A registration's condition
stays an environment-hot key inside the stored `registration` jsonb
document — there is no `condition` column — swept once per sweep and
evaluated per event in the pure decision, before the fire transaction
opens. Neither adds a serving-path read, and seconds-scale filter
repair survives. Admission validates
input against the admitting pod's manifest and writes the durable run
+ queue row — the write-ahead row is the crash floor and idempotency
anchor, and the only reason admission and execution are distinct
moments. Warm path: milliseconds apart.

**Claim/execute.** A worker claims (lock → classify → lease — the
never-replay classifier, unchanged) and **records
`(release version, manifest digest)` from its own pod identity onto
the run**. Its guard is transition-constrained, not write-once:
`NULL → value` by the claim, `value → NULL` by the classifier's
pre-effect arm, `value → value'` refused always. The pair is
therefore write-once per claim attempt — a pre-effect reclaim clears
it in the same transaction that re-enqueues, and the next claim
records afresh under whatever release that pod carries.
Resolution is a pure read of the pod's manifest (adjacency gives the
transitive set); plan bytes fetch by digest, verify at transfer,
cache forever. Effect authority verifies: recorded manifest contains
`(flow, plan-hash)` → plan contains node → attempt matches
`(frame, node, occurrence)` → binding → active generation.

**Audit.** run → recorded version → manifest digest → plan hashes →
bytes; every link content-addressed and immutable. Deployment
bookkeeping is one control-plane attestation row written by the
publish pipeline; rollout state itself lives where v2 puts it — etcd,
inspected with kubectl. That row is also the audit rule for what a
release *is*: a digest is **released** iff a deployment attestation
references it, so a candidate manifest (ruling 1) is distinguishable
by attestation absence, with zero schema.

## Version semantics (no pinning)

Runs execute under the **current** release of the claiming pod — the
standard job-queue semantic. Rollout overlap behaves as for any HTTP
service behind a load balancer; drain completes it. **Breaking input
contracts are author versioning events**: publish the new contract
as a new attachment (`/v2/...`), migrate callers, tombstone the old —
existing machinery. Additive evolution is the default. No agreement
knob, no new refusal categories.

## Deleted by this ruling

`run_flow_resolutions` + `resolution.rs` + the five-step claim ·
release-bound resolution and every version-pinning clause · `.2.4`'s
admission-time bundle pin (moves to claim-time recording) · `.8.19`'s
seven project-side relations, head-pointer row, install transaction,
historical-retry semantics, target-mismatch machinery · the
deployed-manifest append-only rule · the artifact fetch API +
dedicated artifact-reader DB role (OCI pulls) ·
`deployment_attestations`' `deployed_resolution_map` (map-only —
ruling 5; no state column exists in the DDL) ·
report-level map-consistency checking · hand-rolled **wasmCloud
host** Deployment manifests (the chart's `runtime.hostGroups[]` carry
them; `Host` CRs are operator-written observed state and carry no
image, replicas, or podTemplate) — wamn-run-worker is not one of
them and is retained as a plain Deployment: no `ClusterHost`, no
heartbeat, and pretending otherwise would be adoption theater ·
**the waker, at M2 adoption** — host/workload scaling is operator
territory (`runtime.hostGroups[].replicas` / workload scalers); the
dispatcher's wake signal becomes a CRD patch; planned deletion with a
named trigger, not immediate.

## Retained, each with its consumer

Write-ahead run/queue row + classifier (crash floor) · bindings and
credential generations in Postgres (per-tenant runtime authorization,
effect-path-checked) · evidence + command ledger in the control DB
(the gate) · one deploy attestation row (publish bookkeeping) ·
claim-time `(version, digest)` recording (audit) · readiness
prefetch + binding gate (rollout correctness) · dispatcher (queue
reconciliation is ours) · OCI availability trade as accepted.

## Alignment costs, accepted

Tracking a fast-moving v1alpha1 CRD surface: the CRDs and the
operator pin **per cluster** — they are cluster-scoped and Helm
installs them once, so every environment in a cluster shares one
operator version — upgraded only on the fork-sync cadence; chart
**values** (host images, groups) vary per environment and per
release; operator PRs reclassified from N/A to tracked; chart
carriage verified at `v2.7.0` (see Fork sync below).

## Demolition plan — deletion without build/test churn

Inventoried at `mvp@5ae0e3ee`. **Owner-confirmed 2026-08-14:
intermediate commits on the demolition branch may be red — not
building, not passing gates.** Green is required exactly once, at the
merge to `mvp` (the Tier D RC validation is that merge's gate).
Default branch name `deploy-simplification` unless the owner renames;
Tier A's freeze list files to the tracker before the branch opens.

**Churn doctrine.** Freeze before demolition — cancel doomed beads so
no more `.9.10`/`.5.14`-class work lands into the blast radius (the
artifact-reader plane — 19 files including its 3 guards, across
`7ac999b4` + `11aa572b` — landed *this week* and is already superseded). Guards die with their subjects in the same
commit — a surviving guard is what forces a retest. Registries
(`protected-writes.json`, `state-owners.json`, gate-registry, mutant
baselines) regenerate **exactly once**, at wave end. No per-commit
gate runs on the demolition branch; one RC-style validation at merge.
One bead per subsystem, never per file.

**Tier A — cancel, zero code exists (no churn by construction).**
`.8.19` step-B relations/install choreography (verified unlanded) ·
`.8.22` · the plan-bytes/manifest-residency aspects of `.8.18` only —
the management→control authoring/report move itself proceeds (it is
the retained gate); its refusal literal re-anchors off `.8.19`/`.8.22`
· every artifact-fetch follow-on bead · resolution-map evidence
extensions. Freeze list recorded in the tracker first
(`wamn-0h0g.15.1`).

**Tier B — pure deletions, no replacement required (red-safe,
individually mergeable).**
Report-level map-consistency check (a plpgsql trigger in
`deploy/sql/authoring-tests.sql` + its scenario-worker orchestration
caller; the `tested_resolution_map` evidence column is retained) ·
`deployment_attestations`' `deployed_resolution_map` (map-only per
ruling 5; store is days old, no consumers) · superseded doc sections
via one read-through amendment commit. Hand-rolled host manifests are
NOT in this tier: on the Deleted list's scoping the delete set is
empty —
`runner.yaml` is wamn-run-worker's retained plain Deployment
(rewritten, not deleted, for the release-identity mount and
readiness), `scenario-worker.yaml` is no host either, and
flow-http/materializer are already `WorkloadDeployment`-shaped.
The waker is NOT in this tier — its deletion is post-wave at M2
adoption per the Deleted list's named trigger (`wamn-0h0g.15.26`,
dep the CRD-patch wake replacement `wamn-0h0g.15.19`).

**Tier C — coupled delete+replace, one demolition-then-green wave.**
These share files and cannot delete independently without churn;
they land as one branch with a single green-up:
1. *Claim path*: `resolution.rs`, `run_flow_resolutions` DDL +
   functions, five-step → lock/classify/lease, map references in
   `transitions.rs`/`effect_writer.rs`/`sql.rs`, both postgres claim
   modules (`claims.rs`, `production_claim.rs`), queue tests,
   `run_plane.rs` carrier rows; also run-state `src/lib.rs` +
   `tests/run_state_live.rs`, conformance `state_ownership.rs` +
   `schema_drift.rs`, `runnerbench.rs`, ctl `run_plane_live.rs`, and
   guard `global-fifo-claim.sh` (`plan-supply.sh` dies in item 3).
2. *Pin → claim-time recording*: 24 `execution_bundle_hash` sites in
   `run-state.sql`, trigger arm, admission builders; two
   transition-guarded columns added on the existing claim write.
3. *Artifact reader → OCI pull*: the `.9.10`/`.5.14` plane (19
   files including the `artifact-reader-credential.sh` +
   `control-artifact-reader.sh` + `plan-supply.sh` guards), the
   reader DB role and its Secret, readiness rewrite to
   fetch-by-digest.
4. *Manifest reads*: flowrunner map-consumption → mounted-manifest
   resolution; flow-http/materializer route/registration source (the
   flow-http leg is the *first* host-side `wamn:flow-http-routing`
   implementation — no host impl exists today; the materializer
   registration sweep is the real DB-read rewrite).
5. *Test contract*: `authoring_test_sets` store (both planes) +
   four-family parser → in-draft `cases` + flat expect shape;
   `test-set-run` payload drops; report diff shape unchanged
   (`wamn-0h0g.15.27`).
Effect authority's verification set was not merely reworded. Wave
commit 1 **removed** link 1 of the five-link chain above — the
`run_flow_resolutions` EXISTS predicate that bound the guest-declared
plan hash to the run — and nothing replaced it, so the run-to-plan
binding is presently unverified by the database. `wamn-0h0g.15.66`
restores a run-scoped predicate against the recorded manifest digest
and re-anchors the `current-plan-effect-authority.sh` mutant on it;
it **blocks the merge gate** `wamn-0h0g.15.25`.
`effect-writer-primitive.sh` survives with a one-line predicate
update.

**Tier D — regenerate once, at wave end.** Registries + mutant
baselines (inline `EXPECTED_SHA` constants across the 26
baseline-carrying `tools/gate-mutants/` scripts) + the live-battery
deltas per ruling 6 (the `.11.13` bootstrap role probes and the
run-state live suites — reader probes/grants removed, the two
claim-time columns covered; "p0 battery" as a name retires with the
archived `p0-*` docs) + the charter read-through amendment
(`docs/scope-reduction-mvp.md`) + **the fork pin** (below) + the
single RC validation — including the fork-sync gate subset
(socketguard, egress-escape, trace, busyloop epoch) — then
merge to `mvp`.

**Explicitly not deleted in this wave** (floor, unchanged): the
write-ahead run/queue row, the reclaim classifier, the effect
ledger + writer generations, bindings/generations, evidence + command
ledger, the dispatcher, frame execution, budgets.

## Test contract simplification (owner-ratified 2026-08-16; supersedes parts of `.8.1`/`.8.2`; record `wamn-0h0g.13.44`, implementation `wamn-0h0g.15.27`)

The unconditional gate stands — agent-written flows make it *more*
load-bearing. What simplifies is the contract the gate enforces, by
three deletions:

**One expect shape, no families.** Assertion vocabulary flattens to
contract-test form (the industry pattern for gating generated code —
golden input → expected observable):
`expect: { outcome: responded|failed, status?, body_subset?,
failure_code? }`. The four named assertion families delete from the
API. The named-node family (white-box in a black-box gate) is cut —
its authoring-time job is already `draft-run` + capture + `get-run`;
demand-gated if a consumer ever names itself. Reopens `.8.2`.

**Tests live in the draft.** A `cases` array sits beside the graph in
the flow document — unit tests in the source file. One document for
an agent to emit; one draft hash covering flow **and** tests (the
evidence weld strengthens: two-artifact lineage becomes one);
successor-draft copies carry tests forward for free. The separate
test-set store (`authoring_test_sets`, its size cap, hash, FK)
deletes; `test-set-run` drops its payload and means "run the draft's
cases"; the wire contract loses an input type. Reopens `.8.1`;
demolition-compatible — the store deletes in Tier C before more
accretes around it.

**Retained, each with its consumer:** the accepted→poll async shape
(suite length) · ordinal/reservation internals (orchestration guts,
invisible to the API) · the machine-diffable per-case
expected/actual report (the agent's repair loop) · the publish weld —
green report FK'd to the draft hash, which now covers tests by
construction.

Rejected: tests-as-flows (assertion via transform/conditional;
re-imports taxonomy), CI attestation (degrades the gate's claim to
"their runner said so" — wrong buyer, wrong author), sync-only test
execution (dies on suite length).

The agent loop after this ruling: emit one document → `test-set-run`
→ read the diff → fix → repeat.

## Fork sync — wamn/2.7.0 (assessed 2026-08-14; include)

`dkkloimwieder/wasmCloud` branch `wamn/2.7.0` (head `daba602`) is fit
to pin: one `--no-ff` merge of `v2.7.0`, then re-expression commits
`g2br.14–19`; `wamn/2.6.1` stays frozen and reachable. Every
load-bearing patch survived the port — trace seam (`g2br.4`) alive in
`host/http.rs` + the new pooled `http_client.rs`; epoch/memory policy
(`g2br.2/3`) in `engine/ctx.rs`; socket denials re-expressed against
upstream's centralized socket decision point with `g2br.15` re-gating
the plugin raw-socket path; plugin layer adapted to #5411/#5452.
`g2br.16`'s per-run isolation kill-switch patches
`engine/instance_pool.rs` directly — **blocking audit B (instance
pooling vs per-run isolation) is resolved at the source**. Nothing is
droppable yet (fork grew 1,266→1,714 lines / 12→27 files);
`g2br.14` (upstream egress-state leak fix) and `g2br.19` (test
hygiene) are **upstreamable** — file PRs to shrink the 2.8 sync.
wasmtime target **47.0.3** — same family as our 47.0.1, patch-level,
epoch API untouched; `wasmtime_source_identity` re-verifies on pin.
`charts/runtime-operator` present at the tag (carriage verified).

**The pin rides this wave's green-up** — one `ExecutionRuntimeRevision`
bump, one provider-manifest regeneration, one
revalidate→republish→drain loop, instead of two in one week. Gates
before the pin commit: **(A) pool-partitioning audit** — verify
`connection_http`'s per-generation credential injection keys or
bypasses the upstream connection pool (a pooled connection reused
across generations breaks exact-generation authority silently);
**(B)** the five native plugins recompile against the changed plugin
API with the no-trap error-mapping re-audit (#5452). Pin:
`wash-runtime` rev → `daba602`, lock moves to wasmtime 47.0.3.

## Open verification items

Tracked as spikes `wamn-0h0g.15.2` / `.15.3` / `.15.4`:
whether the `Artifact` CRD can carry plans/manifest (data, not
components) or they remain plain OCI pulls in our host code ·
namespace-per-environment mapping under the operator's tenancy
assumptions · whether flowrunner ships as an operator-scheduled
`Workload` (revision changes without host-image rebuilds) or stays
in-image for MVP (provider changes force rebuilds regardless).

## Ratification rulings (owner, 2026-08-16 — decision `wamn-0h0g.13.43`)

1. **Draft/test execution under no-pinning — platform-primitive
composition.** Test-set-run materializes the candidate manifest
(released set + candidate overlay — the ratified bootstrap rule as
data) as a scratch **ConfigMap**, runs a per-report **Job** whose pods
mount it, and targets claims via the landed `.5.9` placement seam
(`execution_target_id` routes the report's cases to the scratch
claimant — no new routing). "Runs execute under the claiming pod's
release" holds verbatim: the candidate *is* that pod's release.
Teardown per the M1 scratch discipline; `draft-run` rides the same
mount. No pin exception exists; post-publish testing is rejected — it
inverts the gate ordering.

2. **`catalog.execution_bundles` — keep.** It remains the
authoritative append-only bytes home; the OCI push is a distribution
projection, idempotently re-derivable from it. Delta-zero: the control
DB already holds this artifact class (test-set bytes, evidence) under
the same append-only + `CHECK` discipline, and the mint transaction
welds evidence to bytes with local FKs. OCI-as-sole-home is
demand-gated, not forbidden. `.12.1`/`.12.8`/`.9.11` unchanged.

3. **"Cache forever" = process-lifetime immutability** — the kubelet
image-cache semantic: no invalidation exists because nothing
invalidates; the cache dies with the pod; readiness prefetch covers
cold starts. The disk-cache deferral (`.13.41`) stays held.

4. **Runtime-revision coherence — construction, not detection.** No
readiness revision-check ships. The release manifest deploys as an
**immutable ConfigMap named by digest** (`release-manifest-<64 hex>`:
a Kubernetes object name is a DNS-1123 subdomain and cannot contain a
colon, so the `sha256:` prefix is stripped), referenced by that name
in the pod template — manifest and image are therefore atomic per pod
by definition; skew has no window to exist in. Its value is the
canonical bytes exactly, with no trailing newline: the reader
re-canonicalizes before comparing digests, so anything else is a
refused mount — a hard constraint on the GitOps writer.
Revision-triple coherence inside the artifact is the mint-time
gate's job. This is the `.2.3`/`.2.7` successor rule: pod
self-identity suffices *because the manifest is part of it*.

5. **Attestation trim is map-only.** Drop `deployed_resolution_map`
(derivable: digest → manifest → map). Keep the six-part coordinate
(the control table observes many targets), `deployed_manifest_hash`
(the audit anchor), and `attested_at`.

6. **Battery deltas target the two live homes**: the `.11.13`
bootstrap role-probe check (artifact-reader probes drop with the role)
and the run-state live suites (`protected_relations_live` +
effect-writer: reader grants gone; the two claim-time columns under
the runs immutability arm). "p0 battery" as a name retires with the
archived `p0-*` artifacts.

Planning decisions folded at ratification: the waker deletion is
post-wave at M2 adoption (named trigger `wamn-0h0g.15.26`, dep the
CRD-patch wake replacement `wamn-0h0g.15.19`) · `.8.18` is partial
(the management→control authoring/report move proceeds as the retained
gate) · all tiers land on branch `deploy-simplification` with a single
green-up at the merge · adoption is fresh beads
(`wamn-0h0g.15.15`–`.15.17`, `.15.19`) superseding
`wamn-x09`/`wamn-6s1`/`wamn-d8i`/`wamn-fqg.40`.

**Addendum (same day):** the test-contract simplification section is
owner-ratified with this spec — record `wamn-0h0g.13.44`,
implementation `wamn-0h0g.15.27` (Tier C item 5). Its ruling extends
the freeze clause updates: `.8.5` (cases source = the draft), `.8.18`
(the test-set-bytes leg of the move deletes), `.7.4` (the wire
contract loses `TestSetInput`). Note ruling 1's test path composes
with it unchanged: `test-set-run` now runs the draft's `cases`
through the same scratch candidate-manifest Job.

Net new machinery across the rulings: one Job kind already in use,
one ConfigMap-per-digest convention, zero daemons, zero roles, zero
protocols — and one mechanism deleted (the readiness revision check).
