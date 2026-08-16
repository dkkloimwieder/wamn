# Charter amendment — plane residency: gate artifacts on the control plane; project databases hold runtime projections and references (owner-ratified revision 2)

Amends: `docs/scope-reduction-mvp.md` (cut 5 ownership/publication/
retention; cut 4 bundle storage) and reads alongside
`docs/flow-execution-amendment.md` · owner-directed 2026-08-12,
branch `mvp`, tracker `wamn-0h0g` · standalone; the charter is read
through this amendment. Status: **owner-ratified 2026-08-14 by
`wamn-0h0g.13.39`; normative for the platform-hosted MVP.** Revision
2 folds the external review: four defects
conceded (cross-plane atomicity, runtime projections, cross-plane
admission idempotency, retention/customer overreach), two remedies
replaced with smaller shapes (no deployment saga; no preflight
protocol), terminology corrected to **control plane**.

> **Deployment-simplification amendment (owner-ratified 2026-08-16 by
> `wamn-0h0g.13.43`; the test-contract clauses by `wamn-0h0g.13.44`).**
> Read this document through `docs/deployment-simplification-spec.md`:
> the runtime-operator manages hosts and workloads as Kubernetes CRDs,
> everything immutable ships as **OCI artifacts**, GitOps converges
> desired state, and **no run is version-pinned** — a run executes
> under the release its claiming pod carries and records
> `(release version, manifest digest)` write-once at claim. The
> sections marked below are superseded in whole or in part; every
> unmarked section stands. Two of this document's rulings are
> **explicitly affirmed** by the spec: control-plane
> `execution_bundles` remains the authoritative append-only bytes home
> (ruling 2 — the OCI push is an idempotently re-derivable
> distribution projection), and verified disk caching stays held by
> `wamn-0h0g.13.41` (ruling 3). A third is **untouched rather than
> affirmed**: the spec is silent on the private management admission
> authority below, which therefore stands on its own ratification
> (`wamn-0h0g.7.5`), not on anything this ruling says.

## Ruling

> **One clause superseded — deployment simplification
> (`wamn-0h0g.13.43`, ruling 4).** "A project database stores the
> **applied runtime projection** of an immutable release" does not
> survive: the applied runtime projection *is* the immutable release
> manifest — RFC 8785 bytes pushed as a content-addressed OCI artifact
> and deployed as a digest-named ConfigMap, read from the pod's mount
> rather than installed into project relations
> (`docs/deployment-simplification-spec.md:20-22`, `:35-39`, `:54-55`,
> ruling 4 at `:323-330`; `wamn-0h0g.15.13`, `.15.14`). Everything
> else in this section stands: control-database residency for the
> authoring, gate, release, identity, and executable-plan objects and
> the "no draft/test-set/report/evidence/plan blobs project-side"
> rule; environment-owned bindings and activation state and the run
> and application planes staying project-side; deploy-and-activation
> as the pull trigger, fetch-by-digest, verify-at-transfer, and the
> memory cache; the write-time hash CHECK on the artifact store
> (ruling 2 keeps `catalog.execution_bundles` authoritative); and CI
> as an actor that owns nothing durable.

**Portable authoring, gate, release, identity, and executable-plan
objects live in the control database. A project database stores the
applied runtime projection of an immutable release, environment-owned
bindings and activation state, and the run and application planes. It
stores no draft, test-set, report, release-evidence, or
execution-plan blobs.**

This simplifies every project database — platform-hosted first:
fewer relations, fewer grants, a smaller protected-write surface,
less to reconcile and drift-check — and it is the standard industry
shape, stated precisely: **deploy and activation trigger the pull**;
the runtime fetches by digest once, verifies at transfer, and caches —
per-run execution reads memory, never the store. Build-time
verification (cargo checksums, git SHAs) happens at publish via the
write-time hash CHECK on the artifact store; transfer-time
verification (container pulls) happens at executor fetch. CI is an
actor that calls the control plane; it owns nothing durable.

## Plane boundary

> **Partly superseded — deployment simplification (`wamn-0h0g.13.43`;
> test-set row by `wamn-0h0g.13.44`).** The plane split stands; eight
> cells carry a supersession. *Project side:* the deployed release
> header + manifest hash, the flow → plan-hash / source-artifact /
> callable-contract references, the call-edge adjacency, and the
> **event registration** half of the registration/causation cell are
> no longer project relations — they are fields of the immutable
> **release manifest** (RFC 8785 bytes, `sha256:<digest>`), deployed
> as a digest-named ConfigMap and read from the pod's mount (ruling 4;
> `wamn-0h0g.15.13`, `.15.14`). The request-attachment
> route/auth/limit/input-mapping projections are served from that same
> mount. A run's release identity is the write-once
> `(release version, manifest digest)` recorded at claim, not a
> project-side header row. `run_flow_resolutions` is deleted outright
> ("Deleted by this ruling"; `wamn-0h0g.15.10`). *Control side:*
> `deployed_resolution_map` drops from the attestation cell (ruling 5
> is map-only — the six-part coordinate, `deployed_manifest_hash`, and
> `attested_at` stay; `wamn-0h0g.15.8`), and "test-set bytes" dies
> with the separate `authoring_test_sets` store — cases live in the
> draft, so the drafts cell carries them (`wamn-0h0g.13.44`,
> `wamn-0h0g.15.27`); the reservations, the report's per-case map, and
> reports are unchanged. Every other cell — principals/roles/PATs, the
> authoring command ledger, immutable definitions, tested evidence
> incl. `tested_resolution_map`, `execution_bundles`, artifact
> retention, bindings + active generations, attachment activation +
> tombstones, causation projections, the run plane, and application
> data — stands. The release→generation retention rows keep their
> residency but lose their writer; see Retention below. The paragraph
> after the table narrows the same way: "routes, registrations,
> bindings, activation state already live project-side today and stay"
> reads — routes and event registrations are projected into the
> release manifest and served from the pod's mount
> (`docs/deployment-simplification-spec.md:54-55`); bindings and
> activation state stay project-side.

| Control plane | Project database |
|---|---|
| Principals, roles, PATs | Applied project schema + migration state |
| Drafts + authoring command ledger | Deployed release header + manifest hash |
| Test-set bytes, reservations, case map, reports | Flow → plan-hash / source-artifact / callable-contract references |
| Immutable flow/release definitions | **Call-edge adjacency** (flow → callee flows, materialized at deploy) |
| Tested release evidence (incl. `tested_resolution_map`) | Request-attachment route/auth/limit/input-mapping projections |
| Execution-plan bytes (`execution_bundles`, the single artifact store) | Event registration + causation projections |
| Deployment attestations (incl. `deployed_resolution_map`) | Connection bindings + active-generation references (+ release→generation retention rows) |
| Artifact retention (append-only) | Attachment activation + tombstones |
| — | Run queue, runs, `run_flow_resolutions`, node/effect facts, operator actions |
| — | Application data |

Runtime projections are not gate artifacts: routes, registrations,
bindings, activation state already live project-side today and stay.
The control plane owns their portable source definitions; deploy
materializes the exact bounded projection runtime needs.

## Publication and deploy — three idempotent steps, no saga

> **Partly superseded — deployment simplification (`wamn-0h0g.13.43`).**
> **Step A stands** and remains the gate: verify draft + green report
> + plan bytes + `tested_resolution_map` → mint the immutable release
> + tested evidence, idempotent on (draft, report); a release is never
> reminted. **Step B is deleted** (`wamn-0h0g.15.14`). There is no
> project-database install transaction. Publish instead pushes the
> content-addressed OCI artifacts — plan bytes and the **release
> manifest** — and writes the environment's desired state to the
> GitOps source: the release-identity ConfigMap
> `(release version, manifest digest)` and any changed CRDs; Argo/Flux
> (plain `kubectl apply` in dev) plus the runtime-operator converge
> it. Deleted with B: the deployed-manifest install and its
> append-only rule, the project-side reference/adjacency/projection
> residency, the target-local runtime-revision verification (ruling 4
> ships **no** revision check — the manifest is an immutable ConfigMap
> named by digest and referenced by that name in the pod template, so
> manifest and image are atomic per pod and skew has no window), the
> coordinate-conflict refusal, and `.8.19`'s install choreography.
> Binding verification survives as the **readiness gate**, not a
> publish-time project transaction: a pod is Ready only once the
> manifest and its referenced plans are fetched and hash-verified
> **and** every connection requirement of the release is bound in this
> environment. **Step C stands minus one column** —
> `deployed_resolution_map` drops (ruling 5, map-only;
> `wamn-0h0g.15.8`) — and stays one plain attestation row keyed
> UNIQUE (release identity, project-env). Consequently the
> coordinates paragraph below keeps A's and C's and loses B's; its
> refusal sentence reads, for the two surviving transactions, plan /
> evidence / manifest-hash content only. In the tested-vs-deployed
> split, `tested_resolution_map` in step A's evidence is **retained**
> and the `deployed_resolution_map` half is deleted. Where B wrote the
> release→generation retention rows atomically with the deployed
> manifest, this ruling names no replacement writer — an open
> obligation of the wave, not a decision made here.

`publish` remains one client operation, implemented as three
convergent idempotent steps keyed by immutable identities. **No
lifecycle object, no pending/active/failed states, no reconciler** —
retries converge because every step is keyed and re-runnable:

```text
A  control txn   verify draft + green report + plan bytes +
                 tested_resolution_map → mint immutable release +
                 tested evidence, idempotent on (draft, report);
                 retry returns the existing release — a release is
                 never reminted
B  project txn   idempotently install the deployed manifest
                 (references, adjacency, runtime projections),
                 verify target-local bindings + active generations +
                 runtime revision, insert the release→generation
                 retention rows, activate attachments + release —
                 keyed by release identity, re-runnable, refusal on
                 coordinate conflict
C  control txn   insert one plain attestation row
                 { release, project-env, deployed manifest hash,
                   deployed_resolution_map, attested_at }, keyed
                 UNIQUE (release identity, project-env)
```

The exact idempotency coordinates are: A = tenant-scoped immutable
validated-draft identity plus finalized-report identity; B = project
environment plus immutable release identity; C = immutable release
identity plus project environment. An exact retry returns the existing
fact. Reusing any coordinate with different plan, evidence, manifest,
projection, manifest-hash, or resolution-map content refuses before
mutation. `attested_at` is immutable row content, never part of C's
idempotency key.

`tested_resolution_map` belongs to release evidence (step A);
`deployed_resolution_map` belongs to the deployment attestation
(step C) — they are facts of different events and are not written in
one transaction. Cross-plane references remain identity-carrying
facts, never foreign keys (the standing one-database-per-env rule).
Separate PostgreSQL databases share no transaction even on one
instance; nothing in this design pretends otherwise.

## Claim — unchanged in shape, bytes fetched under the lease

> **Superseded — deployment simplification (`wamn-0h0g.13.43`).** The
> claim transaction described here is deleted (`wamn-0h0g.15.10`). The
> claim is **lock → classify → lease** — the single-shot reclaim
> classifier is unchanged and retained — plus one **write-once record
> of `(release version, manifest digest)`** taken from the claiming
> pod's own identity, under the existing immutability trigger
> (`wamn-0h0g.15.11`). Resolution is not a claim step and writes no
> rows: it is a pure read of the pod's mounted release manifest, whose
> call-edge adjacency gives the transitive set (`wamn-0h0g.15.13`).
> Deleted with the five-step form: the project-local adjacency read,
> the in-transaction manifest / callable-contract / binding /
> host-runtime-revision verification (ruling 4), and the
> `run_flow_resolutions` insert. **Retained, re-sourced:** plan bytes
> still fetch by digest, verify at transfer, and cache — the fetch is
> an OCI pull, not a control-database read (see the next section);
> "cache forever" means process-lifetime immutability, the kubelet
> image-cache semantic, with no invalidation because nothing
> invalidates (ruling 3), and the disk-cache deferral
> `wamn-0h0g.13.41` still holds. The failure table below stands with
> "artifact store" read as the OCI registry — its availability is the
> accepted trade — except that the binding/generation/contract refusal
> row moves off the claim: unbound requirements are caught by the
> readiness binding gate, and effect authority verifies recorded
> manifest contains `(flow, plan-hash)` → plan contains node → attempt
> matches `(frame, node, occurrence)` → binding → active generation.
> The three pull moments stand: the readiness prefetch, the M2 wake
> cold fetch (**the waker is not deleted in this wave** — its deletion
> is post-wave at M2 adoption, `wamn-0h0g.15.26`), and the lazy
> claim-time fetch for plans not prefetched.

Deploy materializes the **call-edge adjacency** into the manifest
(extracted from the same closure walk publication already performs),
so the claim transaction computes the transitive flow set from
project-local rows alone. The claim's authoritative state mutation is
one project-database transaction, genuinely unchanged: classify →
resolve against the deployed manifest → verify callable contracts +
bindings + the host runtime revision against B's trusted project-local
runtime-revision projection → insert `run_flow_resolutions` → acquire.
**Plan bytes are fetched after claim, under the lease, before any
guest execution** —
a lease is not a lock; no project row is locked across an artifact
fetch. Failure semantics come from the ratified floor (all
pre-effect):

| Failure | Result |
|---|---|
| Artifact store timeout/unavailable | no execution; run re-enqueues fresh (single-shot pre-effect rule) — retryable |
| Digest referenced by the manifest missing from the store | deployment-invalid, typed infrastructure failure |
| Fetched bytes hash-mismatch | integrity refusal; never execute |
| Binding/generation/contract invalid | typed claim refusal (existing kinds) |

The executor hashes every fetch once and holds bytes in the existing
bounded process-memory cache keyed `(tenant, hash)`. Pulls happen at
three moments, all deploy-adjacent or exceptional: the readiness
prefetch on startup/activation (the schedule-triggered pull —
kubelet's shape), the M2 wake cold fetch, and the lazy claim-time
fetch only for plans not prefetched (background/event flows; a callee
deployed after the executor came up). Steady state, a claim is a
cache hit and the artifact store is not in the loop. **No disk cache
in MVP** (demand-gated, with atomic verified insertion if it ever
ships).

## Cross-plane admission idempotency

`draft-run` and every test-set case now cross a control-plane
reservation into a project-database admission. Every such admission
carries a **stable producer idempotency identity** — `draft-run`: the
authoring command id; test case: `report id + case ordinal` —
enforced UNIQUE at project admission with return-existing-on-retry:
the landed same-key-same-run mechanism extended to management
producers with deterministic keys. The ordinal invariant ("one
ordinal never creates a second run") survives the move as the
composition of two idempotent steps; the control-plane reconciler
recovers `run_id` after a crash. The finalized report copies every
asserted result fact into control-plane evidence; a project `run_id`
is a diagnostic coordinate, never a foreign-key or retention
dependency.

### Private management admission authority

Owner-ratified by `wamn-0h0g.7.5`: the project database has one stable
host-only ACL role for this seam,
`wamn_management_admitter`, with exactly `NOLOGIN NOSUPERUSER
NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS`.
Authentication uses project-environment-scoped LOGIN generations named
`wamn_management_admitter_<scope-hash>_{a|b}`. Each generation is a
member of this ACL role only and may connect only to its target project
database. Issuance and rotation reuse the existing issue → authenticate
→ Secret → verify → revoke A/B lifecycle; the stable ACL role never
becomes LOGIN-capable.

The management process uses that credential to extend the existing
`wamn-run-state` native admission API inside one project-database
transaction. This is not a separate admission RPC or a `SECURITY DEFINER`
path. The API inserts the same ordinary run and queue
facts as every other producer, with the stable management producer
identity above: draft-run admits `capture_mode = 'full'`; a test-set
case admits `capture_mode = 'off'`. An exact retry returns the existing
run. Reusing the producer coordinate with any different admitted fact
refuses before mutation.

The role receives only the relation and column reads needed to classify
that retry and the inserts needed for the ordinary run-plus-queue
admission. It cannot update, delete, or truncate an admitted row after
creation; claim or lease work; write effect-attempt or operator-action
facts; or access unrelated project, application, catalog, or control
surfaces. `wamn_app`, every guest, author SQL,
`wamn_scenario_author`, and `wamn_effect_writer` remain denied this
authority. The role is an admission capability, not another data owner.

## Executor ↔ artifact store boundary

> **Superseded — deployment simplification (`wamn-0h0g.13.43`).** The
> artifact fetch API and the **dedicated artifact-reader database
> role** are deleted ("Deleted by this ruling"; `wamn-0h0g.15.12`).
> Plan bytes and the release manifest are pulled as OCI artifacts by
> digest and verified at transfer, so there is no executor-side
> database read left to authorize; registry pull credentials, not a
> database role, carry that authorization (`wamn-0h0g.15.17`). Deleted
> with the role: its Secret, the separate connection pool, its
> `protected-writes.json` entry (registries regenerate exactly once at
> wave end, `wamn-0h0g.15.22`), and the bootstrap role probes for it
> (ruling 6; `wamn-0h0g.15.23`). The landed plane of
> `wamn-0h0g.9.10` / `.5.14` — 19 files including the
> `artifact-reader-credential.sh`, `control-artifact-reader.sh`, and
> `plan-supply.sh` guards — dies with its guards in the same commit.
> **Retained:** readiness as a probe, rewritten to fetch-by-digest and
> widened — Ready requires the manifest and its referenced plans
> fetched and hash-verified **and** every connection requirement of
> the release bound in this environment; and the **M2 wake cold-fetch
> leg**, because the waker survives this wave (`wamn-0h0g.15.26`). The
> opening sentence survives as a statement about digests: a digest is
> an integrity identity, not read authorization.

**A digest is an integrity identity, not read authorization.**
Executors get a dedicated read-only role granted `execution_bundles`
only — no drafts, reports, identity, or evidence — with structural
tenant scope, a connection pool separate from the run-state pool, TLS
per platform policy, bounded timeouts/retries, and digest
verification on every cache fill. The role and its Secret follow the
wrapper's issue → verify → revoke discipline and enter
`protected-writes.json` on regeneration. **Readiness:** an executor
becomes Ready only after prefetching and verifying the plans
referenced by active synchronous request attachments (a probe, not a
subsystem) — a restarted pod during a store outage is a warm
Deployment with a cold cache, and must not satisfy the warm floor
until it can serve. **M2:** the existing wake check gains the
cold-fetch leg (replicas 0 → new executor → cold artifact fetch +
hash verification → claim → completion); no new gate job.

## Retention

> **Partly superseded — deployment simplification (`wamn-0h0g.13.43`).**
> Append-only, GC-free `execution_bundles` is explicitly affirmed
> (ruling 2: it remains the authoritative bytes home and the mint
> transaction still welds evidence to bytes with local FKs), and
> cross-plane plan GC stays held by `wamn-0h0g.13.22`. One clause
> dies: "B writes the latter atomically with the deployed manifest" —
> transaction B and the project-side deployed manifest are both
> deleted, so the `deployed-release` retention row has no named writer
> under this ruling. The two MVP kinds themselves are not ruled on.

Execution plans are **immutable and append-only in MVP; no
cross-plane plan garbage collection exists** (the already-ratified
rule — the prior draft's "pinned-run reachability" chain is
withdrawn; a future collector requires explicit project-to-control
reference leases or signed reachability reports, demand-gated).
Control-plane report/evidence retention uses local foreign keys;
project runtime references use project-local constraints — including
the project deployed view retaining its own release → generation
references, since control-plane evidence copies protect nothing
project-side. Project generation retention has exactly two MVP kinds:
`active-attempt` and `deployed-release`; B writes the latter atomically
with the deployed manifest. Project-local `replay-seed`, `audit-seed`,
and `release-evidence` kinds do not survive this amendment.

## Corollary: customer-hosted project databases (reduced guarantee)

> **One clause superseded — deployment simplification
> (`wamn-0h0g.13.43`).** In the execution-authority-input list below,
> *which release is deployed* is no longer a project-database fact: it
> is the claiming pod's own release identity — GitOps-converged,
> mounted as a digest-named ConfigMap, and recorded write-once on the
> run — and the routes and registrations a pod serves come from that
> same mount. Which generations are selected, which runs are queued,
> and which effect facts are recorded stay project-side. The reduced
> guarantee, the signed-envelope option, and the deferral to
> `wamn-0h0g.13.42` are unchanged.

Under customer superuser control, **plan-byte integrity remains
independently verifiable** and corruption remains confined to that
customer's environment and credentials — but the project database
remains the tenant-scoped **execution-authority input** (which
release is deployed, which attachments are active, which generations
are selected, which runs are queued, which effect facts are
recorded). Run-state, manifest, binding, audit, and never-replay
guarantees become **attestational** on customer-hosted instances
unless the executor also verifies a control-plane-signed
deployed-manifest envelope — a reasonable future mechanism, not
required for the platform-hosted MVP. The executor-residency versus
attestation-only choice is explicitly deferred to `wamn-0h0g.13.42`;
MVP makes no customer-hosted guarantee beyond this corollary. This is
a reduced guarantee, not an absence of trust, and it is a corollary,
not the motive.

## Explicit non-changes and hard limits

> **One clause superseded — deployment simplification
> (`wamn-0h0g.13.43`, ruling 2).** "Plans-as-OCI-artifacts is
> demand-gated" is reversed: plan bytes and the release manifest ship
> as content-addressed OCI artifacts and are the runtime's fetch
> source (`wamn-0h0g.15.14`, `.15.17`); the deferral recorded at
> `wamn-0h0g.13.40` closed with the ruling. Only **OCI as the sole
> home** stays demand-gated — `catalog.execution_bundles` remains
> authoritative. The rest of this section stands, including the
> single platform-hosted environment and the CDC/catalog-schema
> exclusions.

One platform-hosted environment ships; this changes artifact
residency and the deploy/claim seams, not the BoM. Test execution
stays on platform infrastructure. Byte identity, the deterministic
compiler, the effect ledger, and the run-plane enforcement stack are
unchanged and fully load-bearing wherever the platform hosts. CDC
from customer-hosted databases and catalog-managed schema on
customer-hosted databases remain excluded. Plans-as-OCI-artifacts is
demand-gated.

## Superseded contracts

> **One clause superseded — deployment simplification
> (`wamn-0h0g.13.43`).** Everything this section retires stays
> retired — the project-local store, the single-project-transaction
> publication, the deleted retention kinds, the registry publication
> gate, deployment lifecycle/saga/reconciler machinery, artifact
> preflight, and every cutover path. Its closing sentence does not
> survive: "the convergent A/B/C transactions, project-local claim
> followed by post-lease verified fetch" is replaced by step A, the
> OCI artifact push, the GitOps desired-state write, operator
> convergence, and a lock/classify/lease claim that records the pod's
> release write-once and resolves by reading the mounted manifest.
> "One pre-RC cutover train" now names the `deploy-simplification`
> branch's single green-up at merge (`wamn-0h0g.15.25`).

This ruling supersedes the project-local portable store and
project-local `execution_bundles` as the runtime artifact source; the
single-project-transaction publication description; project
`release-evidence`, `replay-seed`, and `audit-seed` generation
retention; the configurable registry publication gate; deployment
lifecycle/saga/reconciler machinery;
artifact preflight before claim; and every dual-read, dual-write,
mixed-version, or compatibility cutover path. The convergent A/B/C
transactions, project-local claim followed by post-lease verified
fetch, and one pre-RC cutover train replace those contracts.

## Migration scope (one cutover train, before any second environment exists)

> **Partly superseded — deployment simplification (`wamn-0h0g.13.43`).**
> Still current: the move of the authoring store, evidence, and
> `execution_bundles` to the control database — the
> management→control authoring/report move of `wamn-0h0g.8.18`
> **proceeds**, it is the retained gate; `.8.18` is superseded only in
> part: its plan-bytes/manifest-residency aspects and, per
> `wamn-0h0g.13.44`, its test-set-bytes leg die
> (`docs/deployment-simplification-spec.md:143-146`, `:346-347`) — and
> the producer idempotency identities on management admissions.
> Superseded, item by item: "replace the per-project bundle table with
> the deployed manifest" (the manifest is an OCI artifact plus a
> digest-named ConfigMap, not a project relation) · "add steps B and C to
> `publish`" (B is deleted; C loses `deployed_resolution_map`) ·
> "point the executor's plan loader at the artifact-store fetch
> (dedicated role)" (OCI pull by digest; no role, no Secret) ·
> "relocate the release→generation retention rows into step B" (there
> is no B) · the `protected-writes.json` line — manifest, adjacency,
> and the artifact-read role never join it, and every registry and
> guard baseline regenerates exactly once at wave end
> (`wamn-0h0g.15.22`) · the bootstrap journey's artifact-reader role
> probes (ruling 6; the deploy-and-resolve verification itself is not
> ruled on, but its resolve leg becomes a mounted-manifest read). The
> M2 cold-fetch leg stands.

Move the authoring store, test tables, evidence, and
`execution_bundles` to the control database (a logical move in MVP);
replace the per-project bundle table with the deployed manifest
(references + adjacency + projections); add steps B and C to
`publish`; point the executor's plan loader at the artifact-store
fetch (dedicated role) + on-fetch hash + existing cache + readiness
probe; add producer idempotency identities to management admissions;
convert evidence→release FKs to identity columns; relocate the
release→generation retention rows into step B; regenerate
`protected-writes.json` (project surface shrinks; manifest,
adjacency, and the artifact-read role join it); extend the bootstrap
journey with one deploy-and-resolve verification and the M2 check
with the cold-fetch leg.

## Owner ratification record

> **Partly superseded — deployment simplification (`wamn-0h0g.13.43`,
> ratification rulings 1–6, 2026-08-16).** Item 1's table stands as
> marked above. Item 2 stands re-read: `execution_bundles` remains the
> authoritative append-only bytes home (ruling 2) but is no longer the
> runtime's fetch source — the OCI artifact is; only OCI-as-sole-home
> stays demand-gated, and `wamn-0h0g.13.40` closed with the ruling.
> Item 3 loses B: `publish` is step A plus the OCI push plus the
> GitOps desired-state write; "no lifecycle object, saga, or
> publication reconciler" still holds — convergence belongs to the
> GitOps controller and the runtime-operator, not to a publication
> reconciler of ours. Item 4 is superseded: the claim does not
> resolve, does not compare host runtime revisions (ruling 4), and
> inserts no map; it locks, classifies, leases, and records
> `(release version, manifest digest)` write-once, with resolution a
> pure read of the mounted manifest and plan bytes pulled by digest
> from OCI. "There is no preflight protocol" stands. Item 5 stands.
> The management admission authority (`wamn-0h0g.7.5`), the held disk
> cache (`wamn-0h0g.13.41`, ruling 3), and the held cross-plane plan
> GC (`wamn-0h0g.13.22`) are untouched.

`wamn-0h0g.13.39` ratified on 2026-08-14:

1. The control/project residency table above is the exact MVP plane
   boundary; cross-plane references carry identities, never foreign
   keys.
2. Control-plane `execution_bundles` is the sole MVP plan-byte home;
   plans-as-OCI is demand-gated in `wamn-0h0g.13.40`.
3. `publish` is one operation composed from convergent A, B, and C
   with the exact keys and conflict rules above; there is no
   deployment lifecycle object, saga, or publication reconciler.
4. Claim resolves from project-local adjacency and manifest facts,
   compares the host revision with B's trusted local projection,
   acquires, and only then fetches and hash-verifies plan bytes under
   the lease before guest entry; there is no preflight protocol.
5. Customer-hosted run-plane trust and signed manifests are not an
   MVP choice and remain explicitly deferred to `wamn-0h0g.13.42`.

The dedicated management admission authority is ratified above by
`wamn-0h0g.7.5`. Verified disk caching is held by `wamn-0h0g.13.41`,
and cross-plane plan garbage collection by `wamn-0h0g.13.22`.
