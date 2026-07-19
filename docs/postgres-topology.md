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

*Shipped (`wamn-q3n.2`):* the T1 cluster itself — `deploy/wamn-sysdb.yaml`, a 3-
instance HA CNPG `Cluster` bootstrapping an empty `wamn_system` DB, standing up
alongside the T3 pool. `docs/system-cluster.md`. The registry tables + the four
testable invariants (references-only / no tenant data / request-path-free / dev
≠ prod recovery domain) are `wamn-q3n.3`.

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

*Shipped (`wamn-q3n.9`):* the shipped shared cluster (`deploy/cnpg-cluster.yaml`
`wamn-pg`) is reframed as this T3 pool (header + `wamn.tier=trials` /
`component=trials-pool` labels; the live cluster is untouched — the file is
doc-of-intent) and made a first-class **placement target**: `provision-org
--tier trials --pool wamn-pg` records a trials org in `registry.orgs` with both
cluster refs pointing at the pool (`Org::for_pool` — no cluster CRs; the pool
already exists). `provision-project-env` then routes that org's project-env
databases onto the pool via `env.side()` (the wamn-q3n.7 path, now from a
registered row rather than a hand-inserted one). Conversion to a T2 pair is the
tier-move (`wamn-q3n.13`); retiring the legacy `postgres.yaml` gate pod is a
separate concern (`wamn-689`). `docs/provisioning.md`. *(Encoding superseded by
D18, `wamn-8df.3`/`.4`: the registration is now `provision-org --template trials
--pool wamn-pg` — a `placement_kind='pooled'` row plus the org's stamped
`dev`/`prod` policy rows; routing derives via `cluster_of`, not `env.side()`.)*

### T4 — Dedicated-per-env (the regulated promotion tier)
Cluster-per-environment for customers whose compliance regime demands maximal
separation (independent PITR per env, separate upgrade windows even between a
customer's own envs). Same seam, same mechanics as T2, more instances; priced
accordingly. Not the default.

*Shipped (`wamn-q3n.14`):* a dedicated org gives **`canary` its own cluster** —
`<org>-prod` (HA-3) + **`<org>-canary`** (HA-2, a *third* recovery domain with
independent PITR) + `<org>-dev`. This is the property §T4 asks for and the T2
`Env::side` collapse (canary → prod) cannot express, so the model gains a stored
`registry.orgs.canary_cluster` (set **iff** the tier is `dedicated`; a DB
biconditional CHECK + a distinctness CHECK, drift-guarded) and per-env resolution
moves from `Env::side` to `Org::cluster_for_env` — `canary` routes to
`<org>-canary` on a dedicated org, else to prod (standard/trials). `provision-org
--tier dedicated` renders the third `<org>-canary` CR (`--emit-canary`);
`provision-project-env --env canary` routes there with no manual `--cluster`; the
`.13` T2→T4 tier move routes each env to its per-env cluster. Proven by a live
in-cluster dedicated-org standup (prod HA-3 + canary HA-2 + dev-1 — canary's DB
lives on its own cluster, physically isolated from prod). `docs/provisioning.md`
§provision-org T4 dedicated orgs. *(Encoding superseded by D18, `wamn-8df.3`:
the stored `canary_cluster` column, its CHECKs, and `Org::cluster_for_env` are
retired — the T4 property is now a `canary` env policy with its **own**
recovery domain, derived by `cluster_of`; see `docs/deployment-model.md`.)*

## Environments become a first-class platform dimension

> **Encoding superseded (D18, `wamn-8df.3`):** the closed `dev / canary / prod`
> env set and the `Tier` enum described below are re-expressed as **data** — a
> validated env slug resolving a named policy row (`registry.env_policies`) and
> a minimal org placement (`pooled` | `dedicated`), with the cluster derived by
> `cluster_of`. See `docs/deployment-model.md`. The topology (T1–T4) is
> unchanged as deployment reality; the tiers survive as configurations.

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

   *Shipped (`wamn-q3n.6`):* the org **cluster-pair renderer**
   (`wamn_provision::org` — `<org>-prod` HA-per-tier + `<org>-dev`
   hibernation-managed, as `serde_json` CNPG `Cluster` CRs), the `provision-org`
   subcommand (render + emit CRs + idempotent `registry.orgs` upsert as the
   `wamn_system` owner), and a live one-org-pair standup as the gate of record.
   The `Database` CRD + `.spec.managed.roles` adoption above is **recorded here as
   the mechanism `.7` uses** — `.6` renders the cluster shape only and does not
   build the per-project-env renderer. Backup config (a `backup` stanza + object
   store prefix) is deferred to `wamn-e1g`. `docs/provisioning.md`.
2. **`provision-project-env`**: create the project-env database + roles
   (declarative) + privilege step (imperative-lite) on the org's appropriate
   cluster — or the trials pool for T3 tenants.

   *Shipped (`wamn-q3n.7`):* the `provision-project-env` subcommand + the pure
   `wamn_provision::database` renderer (the CNPG `Database` CR:
   `wamn-db-<org>--<project>--<env>` owned by `wamn_app`, `ensure: present`,
   `databaseReclaimPolicy: retain`, optional `connectionLimit`). The target
   cluster is chosen by `registry.org(org).cluster(env.side())` — the one path
   serves a T2 org pair *and* the T3 pool *(since D18, `wamn-8df.3`: derived by
   `cluster_of(org, env_policy)` instead)*. The `Database` CRD creates the DB
   declaratively; the thin imperative step (ensure `wamn_app` `NOBYPASSRLS`,
   `REVOKE CONNECT FROM PUBLIC` / `GRANT`) is emitted SQL. It records
   `registry.projects` + `registry.project_envs` (`upsert_project_sql` /
   `upsert_project_env_sql`, SR2) and emits the credential Secret. The RLS floor
   at this stage is the enforceable *substrate* (`NOBYPASSRLS` + `CONNECT`
   confinement); the per-table `FORCE ROW LEVEL SECURITY` floor is applied at
   catalog-publish (2.4/2.5). Gate of record = a live standup on the T3 pool.
   `docs/provisioning.md`.
3. **`provisionbench`** extends to the org pair + a T3 path; the saga records
   land in the system DB.

   *Shipped (`wamn-q3n.8`):* `provisionbench --mode` (`legacy`/`orgpair`/`t3`/
   `saga`/`all`) — a T2-shaped org (two project-env databases, prod + dev) and a
   T3 trials org (one), each asserting routing / per-database isolation /
   least-priv / per-project-env `Secret` layout + the `registry` rows + a
   provisioning saga (substrate-agnostic: the per-project-env DBs are created via
   the real `.7` builders as a plain-SQL stand-in for the `Database` CRD, and the
   registry / saga live in an ephemeral `wamn_system`-shaped schema). The saga
   builders (`wamn_registry::sql::{create,advance,complete,fail}_saga_sql`, SR2)
   ship here; the orchestrator that drives them through the real subcommands stays
   `10.1`. The physical cross-**cluster** isolation of a real T2 pair (`Database`
   CRs on `<org>-prod` vs `<org>-dev`, which needs the operator) is the live
   org-pair standup gate of record. `docs/provisioning.md`.

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

*Shipped (wamn-q3n.10):* the **logical-dump producer** — `wamn-host
dump-project-env` renders a per-project-env `pg_dump -Fd` CronJob at the tier
cadence (trials daily / standard 6h / dedicated hourly) plus a one-shot Job (the
10.3 export / .13 pre-move snapshot), and `--run-now` dumps + records the dump in
`provisioning.dumps` (system-schema.sql). The object-store upload is *rendered*
but its live execution is deferred to when the shared store lands (e1g owns that
infra) — the `.10` gate proves the artifact restorable substrate-agnostically
(`pg_dump -Fd` → `pg_restore` into a scratch DB). See docs/provisioning.md
§`dump-project-env`. The operator-facing **restore runbook** (below) + backup/
restore gates are wamn-q3n.11.

*Shipped (wamn-q3n.11):* the **logical-dump restore** — `wamn-host
restore-project-env` `pg_restore`s a `pg_dump -Fd` artifact into either a
non-destructive **scratch** database (`wamn-restore-<org>--<project>--<env>`, the
default — the sub-cluster carve-out target) or, with `--in-place --confirm`, over
the live project-env database (`pg_restore --clean`, restore-to-last-dump). It reads
the dump **catalog** (`provisioning.dumps` via `select_latest_dump_sql`) so
restore-to-last-dump needs no manual key; the dump bytes are staged locally until
the shared store lands (e1g). The pure `pg_restore_argv` builder is validated
substrate-agnostically (`WAMN_RESTORE_PG_URL` round-trip + in-place `--clean`
replace) and by an in-cluster restore standup on `wamn-pg`. See docs/provisioning.md
§`restore-project-env`. **Operator restore runbook:** (1) *restore-to-last-dump* —
`restore-project-env` into scratch to verify, then `--in-place --confirm` to cut
over; (2) *whole-cluster / arbitrary-instant PITR* and (3) *scratch-cluster
carve-out* both need WAL/PITR and are **wamn-e1g** (below); the **audit-rewind
caveat** (below) applies to any physical restore, and immutability is delivered by
append-only platform-audit export (8.6), not the tenant database.

*Shipped (wamn-e1g):* the **WAL/PITR producer** — continuous WAL archiving + base
backups to the shared object store (MinIO, `deploy/minio.yaml`; buckets
`wamn-backups` for WAL, `wamn-dumps` for logical dumps) via the CloudNativePG
**Barman Cloud plugin** (`deploy/barman-cloud-plugin.yaml`, pinned v0.13.0 — it needs
its own operator Deployment in `cnpg-system` **and cert-manager** for
plugin↔operator mTLS, both additive installs). `provision-org` renders, per
**backup-enabled** cluster (`prod` always, a dedicated `canary` always, `dev` off —
"T2-dev optional"), three CRs from `crates/wamn-provision/src/backup.rs`: an
`ObjectStore` (per-cluster WAL prefix `s3://wamn-backups/wal/<cluster>`, and the tier
**retention window** as `spec.retentionPolicy` — the PITR-SLA knob: **trials 7d /
standard 14d / dedicated 30d**), a `.spec.plugins` WAL-archiver ref on the `Cluster`
(the deprecated in-tree `.spec.backup.barmanObjectStore` is avoided — removal slated
CNPG 1.31), and a `ScheduledBackup` (a base backup at the tier cadence via
`method: plugin`). Emitted by `--emit-object-store` (apply **before** the cluster —
the plugin references it) / `--emit-scheduled-backup` (**after**). Proven by a live
in-cluster standup: a standard org's `prod` cluster reached `ContinuousArchiving=True`,
took a plugin base backup to MinIO, and a recovery cluster restored to a `targetTime`
**between two writes** recovered exactly the pre-target row (the discriminating PITR
proof — the retention window works to a precise instant). The **.10 dump upload is now
live** too (the dump pod is `initContainer`(`pg_dump -Fd`) + `container`(`mc mirror`
to MinIO); proven by a one-shot dump Job landing `toc.dat` under the derivable key).
Whole-cluster PITR + the scratch-cluster carve-out (below) are now executable; the
formal restore **drill** stays 10.3. `wamn-sysdb` (T1) and `wamn-pg` (T3) get WAL/PITR
by the same renderer at next (re)provision — the shared-cluster guardrail forbids
re-applying the already-running clusters here. See docs/provisioning.md §`provision-org`.

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

*Shipped (wamn-q3n.13):* the **tier-move mechanism** — `wamn-host
move-org-tier --org <id> --target-tier <standard|dedicated>`. The pure core
([`wamn_provision::tier_move`]) validates the move is a strict **upgrade** (the
lattice `trials < standard < dedicated`; a same-tier move is a no-op, a
downgrade is rejected — data never moves *down*) and computes the ordered step
plan. The subcommand reads the org's current placement + project-envs from the
T1 registry and, in **plan mode** (default), prints the ordered runbook — the
exact `provision-org` / `dump-project-env` / `provision-project-env` /
`restore-project-env` invocations + `kubectl apply`s in dependency order; with
**`--flip`**, it executes the final control-plane cutover (the idempotent
`registry.orgs` upsert to the new tier + cluster refs, run **after** the data
move). The steps reuse the built pieces (`.6`/`.7`/`.10`/`.11`); the resumable/
compensating **saga** that would drive the plan automatically is `10.1`'s. One
mechanism serves **both** directions (T3→T2 proven by a live cross-cluster
standup; T2→T4 the same code path, its dedicated-per-env cluster shape completed
by `wamn-q3n.14`). See `docs/provisioning.md` §`move-org-tier`. *(Retired by
D18, `wamn-8df.3`: `move-org-tier` / `tier_move` are removed with the `Tier`
enum — a placement change becomes one case of the unified `copy(src → dst)`
operation with a quiesce+verify cutover gate, `wamn-8df.5`; see
`docs/deployment-model.md` §4.)*

**Tier-move runbook (scheduled operation):** the org's registry row stays on
the **old** tier throughout the data move — `provision-project-env` targets the
new cluster by explicit `--cluster`, and the `--flip` cutover is last, so live
traffic never routes to the new clusters before their data is there. Per env:
`pg_dump -Fd` the current database (`.10`), provision it on the new cluster
(`.7`), `restore-project-env --in-place --confirm` the dump into it (`.11`).
The dump/restore is a **downtime window**; the near-zero-downtime alternative is
a **logical-replication cutover** (publication on the source, subscription on
the new cluster, switch over once caught up) — a follow-up; the scheduled window
is the shipped path. `restore-project-env` reuses `pg_restore`; **CNPG
`initdb.import`** (a `bootstrap.import` microservice import on the new
`Database`/`Cluster`) is the documented CNPG-native alternative. A physical
restore carries the **audit-rewind caveat** (§Backup architecture): the moved
`wamn_run` history rewinds with the data; immutability is the append-only
platform-audit export (8.6), not the tenant database.

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
`docs/archive/review-findings.md` R7–R9, plan 10.1/10.3/3.4/8.1/8.6/9.8/9.11.
