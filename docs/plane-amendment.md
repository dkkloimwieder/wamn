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

## Ruling

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

## Executor ↔ artifact store boundary

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

One platform-hosted environment ships; this changes artifact
residency and the deploy/claim seams, not the BoM. Test execution
stays on platform infrastructure. Byte identity, the deterministic
compiler, the effect ledger, and the run-plane enforcement stack are
unchanged and fully load-bearing wherever the platform hosts. CDC
from customer-hosted databases and catalog-managed schema on
customer-hosted databases remain excluded. Plans-as-OCI-artifacts is
demand-gated.

## Superseded contracts

This ruling supersedes the project-local portable store and
project-local `execution_bundles` as the runtime artifact source; the
single-project-transaction publication description; project
`release-evidence`, `replay-seed`, and `audit-seed` generation
retention; `requires_green_suite`, its project override, and publish
policy resolution; deployment lifecycle/saga/reconciler machinery;
artifact preflight before claim; and every dual-read, dual-write,
mixed-version, or compatibility cutover path. The convergent A/B/C
transactions, project-local claim followed by post-lease verified
fetch, and one pre-RC cutover train replace those contracts.

## Migration scope (one cutover train, before any second environment exists)

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

The dedicated management admission authority is a separate decision
owned by `wamn-0h0g.7.5`. Verified disk caching is held by
`wamn-0h0g.13.41`, and cross-plane plan garbage collection by
`wamn-0h0g.13.22`.
