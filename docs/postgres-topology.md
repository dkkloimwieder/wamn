# Postgres topology (D6 refinement, v2) — org clusters, four tiers

**Supersedes** the prior D6-topology note (shared-`Cluster`-vs-cluster-per-project
framing). That note's analysis stands — its CNPG facts are incorporated below —
but its option space was built on the wrong tenancy unit. The unit of isolation
is the **customer/org**, not the project: orgs are few, paying, and B2B; a
Postgres instance per customer is trivially priced into an industrial contract,
and "your data is on your own cluster" is a sentence that closes procurement.
Projects and environments are structure *within* an org.

- **Issue:** wamn-o7v `[2.3/D6]` (decision spike). **Gates** wamn-e1g (WAL/PITR —
  backup shape is set by this note) and the provisioning rework (below).
- **Substrate:** D6 — CloudNativePG in-cluster (wamn-dxi, chosen-revisitable).
- **Shipped baseline being amended:** one shared `Cluster` `wamn-pg`,
  database-per-project, imperative `provision-project` (`docs/provisioning.md`).
- **New platform dimension introduced:** environments. The control-plane model
  becomes **org → project → env (dev / canary / prod)**; see §Environments.

## Hard facts the design is built on (carried from v1, verified 1.26 LTS → 1.29)

1. **CNPG backup/PITR is whole-instance physical, never per-database.** WAL
   archiving + base backups capture the entire data directory; `recoveryTarget`
   picks *when*, never *which database*. No native per-DB PITR; the `Database`
   CRD explicitly does not manage backups. Recovery is never in-place — it
   bootstraps a new cluster.
2. **Per-database logical copy is the carve-out primitive:** `pg_dump -Fd` /
   `pg_restore`, or CNPG `initdb.import` (microservice bootstrap).
3. **Declarative surface:** `Database` CRD (GA 1.25; extensions/schemas 1.26)
   manages CREATE/ALTER DATABASE incl. `connectionLimit`; `.spec.managed.roles`
   manages roles. **Neither** manages per-DB `GRANT`/`REVOKE CONNECT`/RLS — a
   thin imperative privilege step always remains.
4. **Barman Cloud is plugin-only going forward** (in-tree provider deprecated
   1.26, removal slated 1.31): wamn-e1g builds on the plugin.
5. **Hibernation:** a CNPG cluster can be hibernated (pods gone, PVCs kept) and
   woken declaratively — the cost lever for idle dev clusters.

## The decision driver (restated for the org model)

> **What shares a recovery domain with prod?**

Whole-cluster PITR (fact 1) means everything in a cluster rewinds together.
The v1 question ("is per-project PITR firm?") dissolves — per-*customer* PITR
comes free with org clusters. The question that remains is environment packing:
if dev shares prod's cluster, recovering from a dev mistake rewinds prod — and
dev is where mistakes happen.

## The four tiers

### T1 — System cluster (control plane; exactly one per platform environment)
Holds the org/project/env **registry**, provisioning-saga state (10.1's
orchestrated saga needs exactly-once steps and resumability — Postgres work,
not etcd work), platform RBAC (8.1 builder/admin/viewer — distinct from
*application* users, which live in each project's own system schema),
plan/quota definitions, billing **rollups**, and platform-level audit (org
created, project promoted, env provisioned).

**Exclusions are the design:**
- **No tenant data** — no catalogs, run state, payloads, or application users;
  those live in org clusters. Keeps the system DB tiny, low-churn, and not a
  cross-tenant honeypot.
- **No credentials** (R8b logic, strongest here): the registry stores Secret
  *references*; actual credentials live in K8s Secrets (ESO/vault-backed
  later), resolved by components holding the matching RBAC. Compromise yields
  the org *list*, not the keys to every org's data. The dispatcher's projects
  Secret evolves accordingly: registry rows here (dynamic membership),
  credential references via K8s as today.
- **No request-path reads** (explicit invariant): system-cluster-down means no
  *new* provisioning/promotions/deploys, while every org's gateways, runners,
  and dispatcher keep working — data-plane components carry per-project config
  via workload identity; the dispatcher treats the registry as
  cached-with-refresh (outage freezes registry *changes*, not cron/outbox
  firing). The first quota check placed on the request path breaks this; don't.
- **HA day-one** (2–3 instances) — it is shared infrastructure, unlike org
  clusters where HA is a tier knob. Provisioned by Helm/IaC in Epic 1 (it
  cannot be provisioned by the provisioner it backs). Platform dev/staging/prod
  each get their own.
- **Not the trials pool** (T3). Both are "shared platform Postgres"; they are
  different planes — control-plane state vs. real (trial) tenant data —
  different blast radii, backup postures, security profiles. Two clusters,
  always.

*Deferred, recorded:* a CRD/controller front-end over the registry
(GitOps-idiomatic) is a later ergonomics option; saga state, RBAC, quotas, and
billing don't fit etcd regardless, so this cluster exists either way.

### T2 — Org clusters: the standard tier, **prod/dev split** (two per org)
- **`<org>-prod`**: every project's `prod` env database — and `canary`, which
  is prod-shaped validation before rollout and deliberately shares prod's
  failure domain (industrial change-control framing). Backup/WAL/PITR per the
  org's tier; upgrade cadence owned per-org; HA per contract tier.
- **`<org>-dev`**: every project's `dev` env databases and preview/scratch
  envs. Its own recovery domain — a botched dev migration or a dev-restore
  never touches prod; dev's connection slots, autovacuum, WAL throughput, and
  upgrade timing are decoupled from prod. Reduced backup posture (short WAL
  retention or dumps-only). **Hibernation-eligible** (nights/weekends), which
  roughly halves the marginal cost of running two clusters per org.

Within an org cluster, each project-env database is effectively
single-tenant: the RLS floor there is belt-and-braces (kept — it costs nothing
and covers operator error), while remaining **load-bearing in T3**.

Isolation properties bought at the customer boundary: physical data
separation, org-scoped blast radius, noisy-neighbour is self-inflicted,
**native per-customer PITR** ("restore your prod to 10:00" is a first-class
CNPG operation touching nobody else), per-org backup schedules and object-store
prefixes (a clean data-residency answer), per-org upgrade windows.

### T3 — Trials pool (the shipped shared cluster, demoted and kept)
The existing shared `Cluster` + database-per-project(-env) becomes the
**pre-contract tier**: trials, demos, hobby evaluation. Pooled density where it
belongs — many small idle tenants who haven't paid for an instance. The RLS
floor is what makes this pool safe and is load-bearing here. Per-tenant
restore in this tier is the v1 scratch-cluster runbook (below) or the nightly
logical dump — acceptable for trial data. **Conversion = promotion** to a T2
pair via the seam (§Reversibility).

### T4 — Dedicated-per-env (the regulated promotion tier)
`<org>-<project>-prod` etc. — cluster-per-environment for customers whose
compliance regime demands maximal separation (independent PITR per env,
separate upgrade windows even between a customer's own envs). Same seam, same
mechanics as T2, more instances; priced accordingly. Not the default.

## Environments become a first-class platform dimension

This note introduces env structurally; the plan must follow (amendment to 3.4,
control-plane model, and the registry schema):

- Identity is **(org, project, env)** everywhere the control plane speaks:
  registry rows, provisioning, subdomain routing
  (`<project>--<env>.<org>.wamn.example` or equivalent), dispatcher
  registration, and promotion tooling (3.4 draft→staged→applied; 11.2's
  suites-promote-with-flows) — promotion tooling in particular must know which
  project-envs are *the same application*.
- v0 may implement env as structured naming over project-keyed provisioning
  (the `CredentialProvider` seam already resolves an opaque key → URL), but the
  **registry schema carries the triple from day one** so tooling never parses
  names.
- Default env set `dev / canary / prod`; canary lives prod-side (above);
  preview envs are dev-side and disposable.

## Provisioning rework

`provision-project` splits:

1. **`provision-org`** (new): render the T2 `Cluster` pair CRs (prod: HA per
   tier, backup schedule + org object-store prefix; dev: single instance,
   hibernation policy), wait ready, register in the system DB. Once we are
   managing per-customer CRs, CNPG's declarative surface stops being mere
   ergonomics: **adopt the `Database` CRD + `.spec.managed.roles`** for
   per-project-env DB/role creation (option (d) of v1, now pulled in), keeping
   only the thin imperative CONNECT-revoke/GRANT/RLS step (fact 3). The
   `Database` CRD's declarative `connectionLimit` doubles as per-project-env
   noisy-neighbour governance *within* an org cluster.
2. **`provision-project-env`**: create the project-env database + roles
   (declarative) + privilege step (imperative-lite) on the org's appropriate
   cluster — or the trials pool for T3 tenants.
3. **`provisionbench`** extends to the org pair + a T3 path; the saga records
   land in the system DB.

## Backup architecture (reshapes wamn-e1g)

Two complementary mechanisms, per tier:

| Mechanism | Scope | Answers | Tiers |
|---|---|---|---|
| **WAL + base backups** (Barman Cloud plugin) | whole cluster | disaster recovery; "restore this org's prod to any instant in the window" | T1 (always), T2-prod (always), T4; T2-dev optional; T3 (cluster-wide DR only) |
| **Scheduled per-project-env logical dumps** (`pg_dump -Fd` → object storage) | one database | tenant-scoped restore-to-last-dump (minutes, size-of-one-DB); **and 10.3's project export** — same artifact | all data tiers; frequency is a tier knob |

This resolves the v1 note's unaddressed contradiction with plan 10.3
("backup/restore & project export — critical for industrial procurement"): what
procurement asks for is per-project restore/export *capability* — delivered by
the dump artifact everywhere and by native PITR on T2/T4 — not a specific
topology. **WAL retention window** is the PITR-SLA lever and is a per-tier,
per-org knob e1g must expose; RPO for dump-based restore = dump interval,
likewise a tier knob, stated honestly in contracts.

**The scratch-cluster runbook** (v1's §runbook: bootstrap recovery cluster at
`targetTime` → logical carve-out → drop) survives verbatim but demotes to two
uses: T3 arbitrary-instant restores, and intra-cluster carve-outs on T2 (e.g.
"restore only project A's prod to 10:00 when the org cluster holds projects A
and B" — rewinding the whole org cluster would rewind project B too, so the
carve-out path remains the tool for sub-cluster granularity).

**Audit-rewind caveat (applies to every physical restore):** a project-env
database contains its `wamn_run` schema; restoring rewinds that env's run
history and audit rows with the data. Compliance answer, stated now: the
observability pipeline's copies (Loki, 9.x) survive the rewind, and any
8.6-grade immutability claim is delivered by **append-only audit export**
(platform-audit to the system DB / object storage), not by the tenant database.
e1g documents this in the restore runbook.

## Connection math (D5 interaction)

Org clusters bound the D5 concern instead of concentrating it: hosts × pools
now converge per-org, not platform-wide. Per-host pool caps (D5 as decided)
remain adequate longer; the 9.8 pool-saturation metric is the tripwire,
**per org cluster**; the pgBouncer escalation becomes a per-org decision for
outlier orgs rather than a platform flag-day. T3 keeps the original
concentrated math and is where saturation appears first — watch it there.

## Cost model (replaces v1's table)

| Tier | Marginal infra | Notes |
|---|---|---|
| T1 system | 2–3 pods, small PVCs, once per platform env | flat |
| T2 per org | prod: 1–3 pods (HA tier) + PVCs; dev: 1 pod, hibernation-eligible | linear **in paying customers** — priced into the contract; dev hibernation ≈ halves marginal cost |
| T3 pool | ~2–3 pods total (HA) | flat across all trials |
| T4 per env | T2-prod cost × envs | premium tier, priced accordingly |

The v1 objection ("linear cost, ~1Gi floor per instance, no N where it wins")
was decisive against cluster-per-**project** at N=1000 *projects*; it is
immaterial at N = paying-customers, each of whom funds their pair. The pool
absorbs the long tail of non-paying tenants — the density argument survives,
scoped to where density is actually needed.

## Reversibility — the seam, restated

Unchanged and still the load-bearing guarantee: the 2.2 `CredentialProvider`
resolves an opaque key → `ProjectConfig { database_url, … }`; runtime, plugin,
contract, and gates are substrate-agnostic. Tier moves are re-pointing:

- **T3 → T2 (trial converts):** provision the org pair; per-env logical dump →
  `initdb.import`/restore into the org cluster; flip the registry row; the
  dump artifact is the same one 10.3 export uses. **Not free at the data
  layer:** a dump/restore window or logical-replication cutover for
  near-zero-downtime — promotions are scheduled operations, not no-ops. Say so
  in the runbook.
- **T2 → T4 (regulated upgrade):** same mechanics, per env.
- **T2 growth:** an org outgrowing one prod cluster vertically splits by
  project across additional org-owned clusters — the "cells" pattern, aligned
  to customer boundaries, same seam.

## Recommendation

1. **Adopt the four-tier model**: T1 system cluster (HA, references-only,
   request-path-free — day one, Epic 1); T2 org clusters with **prod/dev
   split** as the standard paying tier (canary prod-side); T3 trials pool =
   the shipped shared cluster, demoted; T4 dedicated-per-env as the regulated
   promotion tier.
2. **Introduce (org, project, env) as the control-plane identity triple** —
   registry schema now, plan amendment to 3.4/routing/promotion tooling filed
   alongside this note.
3. **Rework provisioning** into `provision-org` + `provision-project-env`,
   adopting the `Database`/roles CRDs (v1's option (d), now justified) with the
   thin imperative privilege step.
4. **Reshape wamn-e1g**: Barman plugin WAL/PITR templated per org cluster
   (per-org schedules, prefixes, retention knobs) + scheduled per-project-env
   logical dumps (= 10.3 export artifact) + the scratch-cluster carve-out
   runbook (T3 + sub-cluster T2 cases) + the audit-rewind caveat + restore
   gates in `provisionbench`/a new `backupbench`.
5. **Keep the RLS floor everywhere** — load-bearing in T3, belt-and-braces in
   T2/T4.
6. **Record the invariants** as testable statements: system cluster absent
   from all request paths; no credentials in the system DB (references only);
   no tenant data in the system DB; dev never shares a recovery domain with
   prod in T2+.

## Decision

**Decided 2026-07-13 (owner): adopt the four-tier model above** — T1 system
cluster / T2 org prod-dev pairs / T3 trials pool / T4 dedicated-per-env — with
`(org, project, env)` as the control-plane identity triple. This note supersedes
the v1 recommendation (shared-default + tiered escape hatch); the shipped
database-per-project baseline is **kept, as the T3 pool**, so nothing is
discarded and the `CredentialProvider` seam keeps every tier move a re-pointing.

Implementation is a **self-contained epic, `wamn-q3n`**, to be finished before
other work resumes (no jumping around). Ordering is intra-epic: foundation
(identity triple `wamn-q3n.1`, T1 cluster `.2`, registry + invariants `.3`) is
P1; provisioning rework (`provision-org` `.6`, `provision-project-env` `.7`,
`provisionbench` `.8`, T3 demotion `.9`) and backup architecture (per-org
WAL/PITR `wamn-e1g`, logical dumps `.10`, restore runbooks/gates `.11`) are P2;
tier-move `.13` and T4 `.14` are P3. Recorded in the D6 row of
`docs/platform-plan.md`.

## References

Carried from v1: CNPG backup/recovery, `database_import`, declarative
DB/role CRDs, resource sizing (`cloudnative-pg.io/docs/…`). Additional:
hibernation (`…/declarative_hibernation/`), Barman Cloud plugin
(`…/backup_barmanobjectstore/` + plugin repo). wamn: `docs/provisioning.md`,
`CredentialProvider` seam, D6 row, R8b (credential blast radius),
`review-findings.md` R7–R9, plan 10.1/10.3/3.4/8.1/8.6/9.8/9.11.
