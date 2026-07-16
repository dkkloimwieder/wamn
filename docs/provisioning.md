# Managed Postgres provisioning (2.3)

Standing up a project turns the SQL-emitting E3 crates into a live system:
given a project id, provision a per-project Postgres **database** on the shared
cluster, credentialed for the runtime. The output 2.4 (system schema) consumes
is *a provisioned, credentialed, `wamn_app`-roled, empty project database*.

- **Issue:** wamn-jxm `[2.3]`; **Epic:** E2 Data layer. Unblocks wamn-as5 `[2.4]`
  → { wamn-d8u `[2.5]`, wamn-521 `[POC-DM1]` }.
- **Decision:** D6 — **CloudNativePG** (CNPG), in-cluster, chosen-revisitable.
- **Crate:** `crates/wamn-provision` — the pure core (naming, SQL builders,
  Secret rendering).
- **Subcommand:** `wamn-host provision-project` — the imperative driver.
- **Gate:** `wamn-gates provisionbench` + `deploy/provisionbench-job.yaml`.

## Topology (D6)

> **Refined 2026-07-13 (wamn-o7v → epic `wamn-q3n`).** The topology has evolved
> to a **four-tier model** (T1 control-plane system cluster / T2 per-org prod-dev
> cluster pairs / T3 trials pool / T4 dedicated-per-env) with an
> `(org, project, env)` identity triple. **What this section describes — the
> shipped shared cluster, database-per-project — becomes the T3 trials pool**,
> and `provision-project` splits into `provision-org` + `provision-project-env`.
> See `docs/postgres-topology.md` for the decision and the trade-off analysis.
> **Encoding generalized 2026-07-16 (D18, `wamn-8df.3`):** the closed env/tier
> enums are re-expressed as env **policies** (`registry.env_policies`) + a
> minimal org **placement** (`pooled` | `dedicated`), the cluster derived by
> `cluster_of`. See `docs/deployment-model.md`.

**One shared CNPG `Cluster`, a database per project.** This matches the 2.2
per-project pooling model (`CredentialProvider` → per-project `ProjectConfig`
with its own pool + policy) and today's single-instance shape, and keeps a
managed-Postgres (RDS/Cloud SQL) re-target parallel: swap the substrate behind
the same narrow seam (a `CredentialProvider` + a per-project database URL).

The CNPG cluster (`deploy/cnpg-cluster.yaml`, operator pinned at
`deploy/cnpg-operator.yaml`, CNPG 1.29.2) stands up **alongside** the guardrailed
`deploy/postgres.yaml` pod. The legacy S2–S6 gates + crate live-apply gates keep
running against that pod unchanged; only `provisionbench` targets the CNPG
cluster. Migrating the legacy fixtures onto CNPG is a separate later bead. The
DB-serving path carries **no CPU limit** (the S2 CFS lesson).

## Isolation model

Postgres **roles are cluster-global**, so one shared cluster has **one shared
`wamn_app` role** — the grantee every generated tenant floor (3.2) and every
hand-written schema already targets (`GRANT … TO wamn_app`), created
`NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS`. Cross-project isolation is
therefore **not** at the role level; it is three layers:

1. **per-project database** — a component resolved to project *a* holds a pool to
   *a*'s database only and physically cannot address another project's database
   (Postgres has no cross-database queries — the `provisionbench` isolation
   witness);
2. **per-DB CONNECT** — `CONNECT` is revoked from `PUBLIC` and granted only to
   `wamn_app`, so no unexpected role reaches a project database (defense in
   depth — the primary confinement is routing);
3. **RLS within** — the 3.2 tenant floor confines rows by `app.tenant`.

Per-project **distinct** roles/passwords (stronger credential isolation, so a
leaked credential reaches one project not all) is a hardening follow-up (8.2),
**not** this MVP. The provisioning path is structured so a `CREATE ROLE` hook
slots in (that also serves the dispatch role wamn-286 and the user-SQL role
wamn-1nd).

## `wamn-host provision-project`

An imperative CLI (the `publish-catalog` precedent), run as a Job — not a
Project CRD + controller (that is the 10.1 control plane). It connects as the
cluster **superuser** (only the operator/superuser creates databases and roles)
and runs the pure builders:

| Flag | Purpose |
|---|---|
| `--project <id>` | Project id: a lowercase slug `[a-z0-9-]`, start/end alphanumeric. Maps to database + Secret `wamn-db-<project>`. The reserved `wamn` prefix (wamn-66x) is rejected. |
| `--admin-database-url` / `WAMN_PG_ADMIN_URL` | Superuser URL to the cluster maintenance database. |
| `--app-password` / `WAMN_APP_PASSWORD` | Password for the shared `wamn_app` role (default `wamn_app`, matching the hand-written schemas). |
| `--app-host` / `--app-port` | Runtime-facing host/port (default: the admin URL's). |
| `--emit-secret <path>` | Write the credential `Secret` (JSON manifest; `-` = stdout) — `kubectl apply -f`. |
| `--emit-projects-file <path>` | Write the `WAMN_PG_PROJECTS_FILE` entry (`-` = stdout). |

It is **additive and idempotent** (create-if-absent; the shared-cluster
guardrail): re-running refreshes the grants and re-emits the credential, never
dropping anything. What it does, in order:

1. ensure the shared `wamn_app` role (idempotent `DO` block; pre-created in
   production);
2. `CREATE DATABASE "wamn-db-<project>"` when absent (autocommit — `CREATE
   DATABASE` cannot run in a transaction block);
3. `REVOKE CONNECT … FROM PUBLIC; GRANT CONNECT … TO wamn_app;`

The project id is a slug (not an underscore identifier) on purpose: it is both a
Kubernetes Secret-name suffix (hyphens, no underscores) and — quoted — a
database name. Hyphenated database names are quoted in DDL and are URL-path-safe,
so one slug serves both domains without translation.

## Emitted credential — the 5x0.1 contract

2.3 **emits** the credential; the live in-cluster read stays wamn-5x0.1 `[2.2b]`
(the `K8sSecretProvider` — a stub today). Two shapes, both produced by the pure
crate:

- **`WAMN_PG_PROJECTS_FILE` entry** — `{ "<project>": { "url": "…" } }`, the
  exact shape the plugin's `StaticCredentialProvider` (`from_env`) and the
  dispatcher `--projects-file` already parse. This is how `provisionbench`
  proves a provisioned project resolves through the production code path.
- **Kubernetes `Secret`** — named `wamn-db-<project>` (the key 5x0.1 will look
  up), `type: Opaque`, `stringData.url` = the `wamn_app` connection URL.
  Rendered as JSON (which `kubectl apply -f` accepts), so a Job pipes it straight
  to the API server — no Rust K8s write client is pulled into 2.3 (kept
  symmetric with deferring the 5x0.1 read client).

Policy knobs (`row_limit`, timeouts) are optional in the projects-file entry and
default from the plugin's base config, so the MVP credential carries only the URL.

## `provisionbench` — the gate

A pure host-side `tokio_postgres` gate (no wasm guest — the queuebench /
dispatchbench shape). It provisions **two** projects through the real
`provision-project` path, then asserts:

- **routing / resolution** — each project's emitted credential, parsed through
  the plugin's own `StaticCredentialProvider`, resolves to that project's
  database (a distinct marker witness, 111 / 222);
- **database-level isolation** — a project's connection cannot see another
  project's tables (undefined-table across databases);
- **least privilege** — the resolved `wamn_app` connection is
  `NOSUPERUSER NOCREATEDB`;
- **credential layout** — the emitted `Secret` carries the name + URL 5x0.1
  reads.

Load-bearing asserts are mutation-tested (apply/test/restore, debug builds):
dropping the per-DB `GRANT CONNECT` fails the resolve→connect step here; dropping
the `REVOKE … FROM PUBLIC` fails the `wamn-provision` live-apply gate; neutering
the project-id reserved-prefix check fails a unit test.

The gate is **substrate-agnostic** — it needs only a superuser URL — so it runs
locally against a throwaway `postgres:18` (fast iteration) and in-cluster against
the CNPG cluster (the gate of record).

## `wamn-host provision-org` (wamn-q3n.6, generalized to D18 by wamn-8df.3)

The topology (`docs/postgres-topology.md`) splits `provision-project` into
`provision-org` + `provision-project-env`. **`provision-org`** places an org —
`--placement pooled` (shared pool, record-only) or `--placement dedicated` — and,
for a dedicated org, renders its CNPG `Cluster` **set**: one cluster per
**distinct recovery-domain owner** across the env policies
(`registry.env_policies`), each named `<org>-<owner>` and **sized by its owner
env's policy** (`instances` / `storage` / `cpu` / `memory` / `image` — the cjv.21
fix: sizes are policy-driven, not hard-coded). With the default `dev` / `prod`
policies:

- **`<org>-prod`** — HA (the `prod` policy's 3 instances), pod anti-affinity
  spread, WAL/PITR backed (the policy's `backup_cadence` / `wal_retention`);
- **`<org>-dev`** — a single, **hibernation-eligible** instance (the
  `cnpg.io/hibernation` annotation, set `off` so it comes up ready; the platform
  off-hours scheduler flips it `on`), no scheduled backup (its restore path is
  the logical dump).

Adding a `canary` policy **shared-with `prod`** changes nothing here (canary
co-resides on `<org>-prod` — the old T2 shape); a `canary` policy with its
**own** recovery domain adds a third cluster `<org>-canary` (the old T4 shape,
formerly wamn-q3n.14's special case). HA (anti-affinity) keys on
`instances >= 2`; hibernation on `hibernation: eligible` — both policy knobs.

All clusters carry `enableSuperuserAccess` (the per-project-env path connects as
superuser), a non-TLS `pg_hba`, and **no cpu limit** (the S2 CFS lesson). Cluster
**names** derive from `wamn_registry::cluster_of(org, policy)` — the one rule the
renderer and `Registry::resolve` share, so a provisioned cluster and a resolved
triple always agree. The policies come from `registry.env_policies` when a
system-DB URL is given (an operator-added `canary` policy is honored), else the
built-in `dev`/`prod` defaults.

| Flag | Purpose |
|---|---|
| `--org <id>` | Org id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric). Names the derived `<org>-<owner>` clusters; the reserved `wamn` prefix is rejected. |
| `--placement pooled\|dedicated` | `pooled`: placed on `--pool`, record-only (owns no clusters). `dedicated`: owns one cluster per recovery-domain owner, rendered here. |
| `--pool <cluster>` | The shared pool a `pooled` org is placed on. Ignored for `dedicated`. Default `wamn-pg`. |
| `--system-database-url` / `WAMN_SYSTEM_ADMIN_URL` | Superuser URL to the T1 `wamn_system` DB: read the env policies (cluster sizing) and record the org row. Omit to render/plan only (default policies). |
| `--emit-clusters <path>` | Write the rendered `Cluster` CRs (a JSON `List`; `-` = stdout, the default) — `kubectl apply -f`. Empty for a `pooled` org. |
| `--emit-object-store` / `--emit-scheduled-backup` `<path>` | The WAL/PITR `ObjectStore` / `ScheduledBackup` CRs (JSON `List`s, wamn-e1g) for the backup-enabled clusters. Apply ObjectStores **before** the clusters, ScheduledBackups **after**. |

### Pooled orgs (wamn-q3n.9, generalized)

A `pooled` org shares the pre-contract **pool** (`deploy/cnpg-cluster.yaml`
`wamn-pg` — the T3-style tier, `docs/postgres-topology.md` §T3), so it owns no
clusters: there is nothing to render. `--placement pooled` builds the org via
`Org::pooled(id, pool)`, validates it, records **only** the `registry.orgs`
placement row (`placement_kind='pooled'`, `pool_cluster=<pool>` — the same
idempotent `upsert_org_sql` path), and emits no CRs. `provision-project-env` then
reads that placement and derives the pool via `cluster_of` — no manual
`--cluster`. Conversion to a dedicated org is the unified `copy` move
(`wamn-8df.5`; the retired `move-org-tier` — see below).

### Dedicated placement variants (formerly T2/T4)

The old tier distinction is now a **policy difference**, not a code path
(`docs/deployment-model.md`):

| Old tier | D18 expression | Clusters rendered |
|---|---|---|
| T2 standard | `dedicated` + `canary` policy shared-with `prod` | `<org>-dev`, `<org>-prod` (canary co-resides on prod) |
| T4 dedicated | `dedicated` + `canary` policy with `own` recovery domain | + `<org>-canary` — a third recovery domain with independent PITR |

The wamn-q3n.14 `canary_cluster` column, its two CHECKs, and the
`Org::cluster_for_env` special case are **retired** — subsumed by
`recovery_domain` + the `cluster_of` derivation. `provision-project-env --env
canary` routes to whichever cluster canary's policy derives, with no manual
`--cluster`.

Like `provision-project`, it is a **renderer + DB writer only** — it does NOT
apply the CRs (the runbook/Job `kubectl apply`s them and waits ready, as the
Secret pattern) and does NOT create per-project-env databases. The org row is an
**idempotent upsert** into `registry.orgs` (`ON CONFLICT (id) DO UPDATE`), written
as the `wamn_system` owner; the builder lives with the registry model
(`wamn_registry::sql::upsert_org_sql`, SR2 single-source).

**Scope: the cluster SHAPE + the registry row only.** Per-project-env
database/role creation is `provision-project-env` (wamn-q3n.7). The rework
**adopts the CNPG `Database` CRD + `.spec.managed.roles`** for that declarative
DB/role creation (keeping only the thin imperative `CONNECT`-revoke / `GRANT` /
RLS step CRDs don't cover; the CRD's `connectionLimit` doubles as per-project-env
noisy-neighbour governance).

The gate of record is a **live dedicated-org standup** (the wamn-q3n.2 infra
precedent): render + record via the real subcommand against the T1 `wamn-sysdb`,
`kubectl apply` the cluster set alongside the guardrailed clusters, wait ready,
and assert per-policy sizing (prod HA-3 + anti-affinity, dev single +
hibernation annotation), the distinct plane (own Services / Secrets / PVCs), and
that the live cluster names equal the `cluster_of` derivation. Teardown deletes
**only** the new clusters + row (never `wamn-pg` / `postgres.yaml` /
`wamn-sysdb`).

### WAL/PITR backups (wamn-e1g)

`provision-org` also renders each paying cluster's **WAL/PITR backup** — continuous
WAL archiving + base backups to the shared object store, giving whole-cluster
point-in-time recovery. This is the first of the two backup mechanisms
(docs/postgres-topology.md §Backup architecture); the per-project-env logical dump
is the other (`dump-project-env` / `restore-project-env`, wamn-q3n.10/.11).

**Mechanism = the CloudNativePG Barman Cloud plugin** (`barman-cloud.cloudnative-pg.io`).
The in-tree `.spec.backup.barmanObjectStore` provider is deprecated in CNPG 1.26
(removal slated 1.31), so this builds on the plugin — a CNPG-I sidecar the operator
drives. It is a separate install (`deploy/barman-cloud-plugin.yaml`, pinned v0.13.0,
into `cnpg-system`) and **requires cert-manager** (plugin↔operator mTLS); the shared
object store is `deploy/minio.yaml` (MinIO — buckets `wamn-backups` for WAL,
`wamn-dumps` for logical dumps).

The pure renderers live in `crates/wamn-provision/src/backup.rs`. Per
**backup-enabled** cluster (D18: the owner env's policy has a non-empty
`backup_cadence` — the default `prod` policy is backed, `dev` is not; its restore
path is the logical dump), the org's cluster set carries:

* an **`ObjectStore`** CR — `destinationPath = s3://wamn-backups/wal/<cluster>` (a
  per-cluster WAL prefix, each recovery domain isolated), `endpointURL` = the
  in-cluster MinIO, `s3Credentials` → the shared `wamn-object-store` Secret, and
  `spec.retentionPolicy` = the policy's **`wal_retention`** — the **PITR-SLA
  knob** (the recovery window; the default `prod` policy ships `14d`);
* a **`.spec.plugins`** WAL-archiver ref on the `Cluster` (`isWALArchiver: true`,
  `barmanObjectName` = the ObjectStore) — every WAL segment is shipped to the store;
* a **`ScheduledBackup`** CR — a base backup at the policy's **`backup_cadence`**
  (a 6-field CNPG cron; the default `prod` policy ships 6-hourly) via
  `method: plugin`, `immediate: true` (opens the window at once).

`provision-org` emits these via `--emit-object-store` (a `List`, apply **before** the
cluster — the plugin references the ObjectStore) and `--emit-scheduled-backup` (apply
**after** the cluster exists). Runbook order per backed cluster: `ObjectStore` →
`Cluster` → `ScheduledBackup`.

**Restore / PITR runbook.** To rewind an org cluster to an instant in its window,
bootstrap a **recovery `Cluster`** whose `bootstrap.recovery` names an `externalClusters`
entry that points at the ObjectStore via `plugin` (`barmanObjectName` + `serverName` =
the source cluster) and a `recoveryTarget.targetTime`. The operator restores the base
backup and replays archived WAL up to the target — a targetTime *beyond the last
archived transaction* fails "recovery ended before target reached", so ensure WAL past
the target is archived (`pg_stat_archiver.last_archived_time`). Whole-cluster PITR
rewinds every project on the cluster; for **sub-cluster** granularity (one project on a
shared cluster) carve the one database out of the recovery cluster with a logical dump
(the scratch-cluster runbook, docs/postgres-topology.md). The formal restore **drill**
is 10.3; the **audit-rewind caveat** (a physical restore rewinds that env's `wamn_run`
history) applies.

**The `.10` dump upload is now live** against the same MinIO: the dump pod is an
`initContainer` (`pg_dump -Fd` into a shared volume) + a `container` (the MinIO client
`mc mirror`s the dump directory's contents to `s3://wamn-dumps/<derivable-key>`), the
upload guarded on the S3 endpoint env.

The gate of record is a **live in-cluster WAL/PITR standup**: install cert-manager +
the Barman plugin + MinIO, provision a standard org with backup, apply
ObjectStore → Cluster → ScheduledBackup, confirm `ContinuousArchiving=True` + a plugin
base backup in MinIO, then prove PITR by recovering a cluster to a `targetTime` between
two writes and asserting it recovered exactly the pre-target row (the discriminating
proof); plus a one-shot dump Job landing `toc.dat` under the derivable key. Teardown
deletes **only** the test org's clusters / backup CRs / dump Jobs / objects; the backup
infra (cert-manager / plugin / MinIO) stays as platform substrate, and the guardrailed
clusters are never touched.

## `wamn-host provision-project-env` (wamn-q3n.7)

The per-env counterpart of `provision-project`: **`provision-project-env`**
stands up one per-project-env Postgres database, keyed by the `(org, project,
env)` `Triple`. Identity everywhere; the database lives on the cluster
**derived** by `wamn_registry::cluster_of` (D18) from the org's placement + the
env's policy — a dedicated org's `<org>-<owner(env)>` (so `canary` shared-with
`prod` lands on `<org>-prod`, `canary` own on `<org>-canary`), or the shared pool
for a pooled org. **One derivation path serves every placement** — it does
**not** use `Registry::resolve`, which needs the project-env to already exist.
The subcommand reads the org's placement row (`select_org_placement_sql`) + the
env's policy row and derives the cluster.

**Per-project-env naming.** The database (and the K8s `Database` resource, and
the credential Secret) is `wamn-db-<org>--<project>--<env>`. The **org** is
encoded — unlike the 2.3 `wamn-db-<project>` — because the shared trials pool
hosts many orgs (two orgs' identically-named projects would otherwise collide on
one cluster) and every cluster's `Database` resources share the one K8s
namespace. `--` separates the identity components (the `Triple::host_label`
convention). The assembled name is length-validated (`≤ 63` bytes — a legal
Postgres identifier and DNS-1123 label).

**Declarative DB + imperative privilege step.** The database is created
declaratively via the CNPG **`Database` CRD** (`spec.name` / `spec.owner` /
`spec.cluster.name` / `ensure: present` / `databaseReclaimPolicy: retain` so
deleting the CR never drops tenant data / optional `connectionLimit` = the
per-project-env noisy-neighbour cap). It is owned by the shared least-privilege
`wamn_app` role, so no tenant database is superuser-owned. The thin imperative
step the CRD does **not** cover (topology fact 3) stays SQL: **ensure the
`wamn_app` role** (`NOSUPERUSER NOCREATEDB NOBYPASSRLS`) and **confine `CONNECT`**
(`REVOKE … FROM PUBLIC` / `GRANT wamn_app`).

**RLS floor at provision time.** There are no tables yet, so `.7` establishes the
RLS-**enforceable substrate** only — `wamn_app` is `NOBYPASSRLS` (so RLS is
enforced once tables exist) and `CONNECT` is confined. The per-table `FORCE ROW
LEVEL SECURITY` floor is applied at **catalog-publish** (2.4/2.5), where the
tables are created — load-bearing in T3, belt-and-braces in T2/T4.

| Flag | Purpose |
|---|---|
| `--org` / `--project` / `--env <slug>` | The identity triple. The project id is a slug (reserved `wamn` prefix rejected); the env is any `registry.env_policies` name (default set `dev` / `prod`; `canary`, `staging`, … are addable policies). |
| `--system-database-url` / `WAMN_SYSTEM_ADMIN_URL` | Superuser URL to the T1 `wamn_system` DB: read the org's placement + the env's policy (derive the cluster) and record the project + project-env. Omit + `--cluster` to render only. |
| `--cluster` | Override the target cluster (else read from the registry). |
| `--connection-limit` | The per-project-env `CONNECTION LIMIT`. Default: no limit. |
| `--emit-database` / `--emit-role-sql` / `--emit-privilege-sql` / `--emit-secret` | Write each artifact (`-`/absent = stdout with a labeled header). |

Like the other subcommands it is a **renderer + registry writer only** — it does
not apply the CR or the SQL. It records `registry.projects` + `registry.project_envs`
(idempotent, as the `wamn_system` owner; the builders `upsert_project_sql` /
`upsert_project_env_sql` live with the registry model, SR2) and emits everything
else. **The runbook/Job applies the emitted artifacts in order** (the role SQL
must precede the CR — the CR's `owner` must exist — and the privilege SQL follows
the database's creation):

1. `psql` the **role SQL** to the target cluster (superuser) — ensures `wamn_app`;
2. `kubectl apply -f` the **`Database` CR** and wait `.status.applied=true` — the
   operator creates the database owned by `wamn_app`;
3. `psql` the **privilege SQL** to the target cluster — `REVOKE`/`GRANT CONNECT`;
4. `kubectl apply -f` the **credential Secret**.

**Scope (wamn-q3n.7): the per-project-env DB + role + privilege step + the
registry rows + the Secret.** It does **not** extend `provisionbench` to the org
pair / T3 path (wamn-q3n.8), register the pool as the trials tier (wamn-q3n.9 —
`.7` *routes* a trials org to the pool via `env → side`), emit per-project-env
logical dumps (wamn-q3n.10), or configure WAL/PITR (wamn-e1g).

The gate of record is a **live standup** on the pool (`wamn-pg` is always up):
seed a pooled org, run the real subcommand (which reads the placement → `wamn-pg`
and writes the registry rows), apply the role SQL + `Database` CR + privilege SQL,
and assert the database exists owned by `wamn_app` with the connection limit,
`CONNECT` confined (`PUBLIC` revoked, `wamn_app` granted), the `NOBYPASSRLS`
substrate, the registry rows, and — via a dedicated org rendered without applying
— that `cluster_of` derives `<org>-dev` for `dev` and `<org>-prod` for `prod` (and
routes an added `canary` policy by its recovery domain). Teardown deletes **only**
the new `Database` CR + registry rows, then drops the created database (the
`retain` policy leaves it) — never `wamn-pg` / `postgres.yaml` / `wamn-sysdb`.

## `provisionbench` — the four-tier extension (wamn-q3n.8)

`provisionbench` gains `--mode` (the pgbench / queuebench precedent):

- **`legacy`** — the 2.3 two-project flow above, kept as regression.
- **`orgpair`** — a **dedicated** org (`Org::dedicated`, so `cluster_of` derives
  `<org>-prod` ≠ `<org>-dev`) with two project-envs (`prod` + `dev`) as two
  per-project-env databases (`wamn-db-<org>--<project>--<env>`). Off-cluster the
  CNPG `Database` CRD is unavailable, so the databases are created with plain SQL
  through the **real** wamn-q3n.7 builders (`ensure_app_role_sql` +
  `create_database_named_sql` with the per-project-env name +
  `grant_connect_on_database_sql`) — honest superuser scaffolding, the same shape
  the CRD reconciles to. Asserts per-database routing + isolation + least-priv +
  the per-project-env `Secret` layout, records the D18
  `registry.orgs`/`projects`/`project_envs` rows (placement + the env FK), and
  lands a provisioning **saga** (create → step-per-env → complete).
- **`t3`** — a **pooled** org (every env collapses onto the shared pool) with one
  project-env; the same per-placement assertions.
- **`saga`** — a focused proof of the saga builders: exactly-once create, durable
  step advance, terminal complete + fail.
- **`all`** — `legacy`, then (over one ephemeral registry schema) `saga`,
  `orgpair`, `t3`.

The tier / saga modes need the T1 registry, so the gate applies
`deploy/system-schema.sql` into an ephemeral `registry` / `provisioning` schema
pair on the same PG (dropped at teardown). It is still **substrate-agnostic** — a
superuser URL — so `--mode all` runs locally against a throwaway `postgres:18`.

**Saga builders (SR2, wamn-q3n.8):** `wamn_registry::sql::{create,advance,
complete,fail}_saga_sql` — the exactly-once / resumable state the orchestrator
(10.1) drives; `.8` ships the builders and proves a saga **lands in the system
DB** per provisioned tier, but does **not** wire sagas into the real `provision-org`
/ `provision-project-env` subcommands (that orchestrator stays 10.1). The `status`
literals are drift-guarded against the `provisioning.sagas` CHECK, and a live
exactly-once / step / complete / fail proof is spliced into the wamn-q3n.3 storage
gate. Mutation-tested (apply/test/restore, debug builds): dropping the create
`ON CONFLICT`, neutering the step advance, and a wrong terminal-status literal
each fail a named unit test + the live proof (the step mutant also fails
`--mode saga`).

**The physical cross-CLUSTER isolation of a real dedicated org needs the
operator**, so the in-cluster **gate of record is a live dedicated-org standup**
(the `.6`/`.7` precedent — the debug binary + `kubectl`, no image rebuild):
`provision-org --placement dedicated` renders and stands up a real `<org>-prod`
(HA) / `<org>-dev` set, then `provision-project-env` derives each env's cluster
via `cluster_of` and renders a `Database` CR on **each** (the prod env on
`<org>-prod`, the dev env on `<org>-dev`). It proves each project-env database
lives on a **different Postgres cluster** — `<org>-prod` holds only the prod
database, `<org>-dev` only the dev database — with `wamn_app` owner, `CONNECT`
confined, `NOBYPASSRLS`; plus the same `Database`-CRD path on the pool
(`wamn-pg`). Teardown deletes **only** the new clusters + `Database` CRs (and
drops the retained pool database on `wamn-pg`) — never `wamn-pg` /
`postgres.yaml` / `wamn-sysdb`.

## `wamn-host dump-project-env` (wamn-q3n.10)

The **second backup mechanism** (docs/postgres-topology.md §Backup architecture):
a scheduled `pg_dump -Fd` of one project-env database to object storage. **One
artifact** serves tenant-scoped restore-to-last-dump *and* the 10.3 project
export; the RPO is the dump interval (`--schedule`; D18: no longer a closed-tier
knob — a per-env `dump_cadence` policy field is a future additive column). This is
the *dump producer* — the operator-facing RESTORE runbook + audit-rewind caveat +
backup/restore gates are wamn-q3n.11; the cross-cluster move that consumes a dump
is the unified `copy` (wamn-8df.5); whole-cluster WAL/PITR is the *other*
mechanism, wamn-e1g.

The pure renderer + builders live in `crates/wamn-provision/src/dump.rs` (the
`render_project_env_database` precedent — no clock, no DB, no K8s client):

- `render_project_env_dump_cronjob(triple, schedule, bucket)` — a `batch/v1`
  **CronJob** running `pg_dump -Fd` of the project-env database (connection from
  the project-env credential Secret's `url`, so the target cluster is not named),
  `concurrencyPolicy: Forbid`. The **object-store upload** is *rendered* into the
  container command but **guarded** on the upload CLI being present — the dump
  succeeds whether or not the shared store is wired yet (**Q2**: no store exists in
  the repo; the shared MinIO/S3 lands with wamn-e1g, whose Barman WAL/PITR needs
  the same store);
- `render_project_env_dump_job(triple, bucket)` — a one-shot **Job** (`generateName`,
  `kubectl create -f`) for the on-demand export / .13 pre-move snapshot;
- pure builders: `pg_dump_argv` (`-Fd` **directory format** is load-bearing —
  parallel + selective restore, the one artifact 10.3 reuses), `dump_object_key`
  (`dumps/<org>/<project>/<env>/<timestamp>` — derivable, so restore needs no
  registry read), `DEFAULT_DUMP_SCHEDULE` (daily, 03:00).

`wamn-host dump-project-env` drives them:

```
dump-project-env --org <org> --project <p> --env <slug>
  [--schedule <cron>]                                 # cadence (default daily)
  [--emit-cronjob <path|->] [--emit-job <path|->]     # render the manifests
  [--run-now --database-url <project-env db> --out-dir <dir>
   --system-database-url <sysdb>]                     # dump now + record
```

The cadence is `--schedule` (default `0 3 * * *`). `--run-now` runs `pg_dump -Fd`
imperatively (the on-demand export) and **records** the dump in the T1 registry
(`provisioning.dumps`, via `--system-database-url`).

**Dump bookkeeping (Q3):** `provisioning.dumps` (system-schema.sql) records each
dump — the object key, `format` (`directory`), completed `byte_size`, `taken_at` —
FK'd to the project-env it dumps (a de-provisioned env, or a deleted org cascading
through `project_envs`, drops its dump records). It is control-plane **metadata**,
not tenant data (invariant 3) and holds no credentials (invariant 2); the dump
**bytes** live in object storage and the dump **catalog for restore** is
wamn-q3n.11's. `wamn_registry::sql::record_dump_sql` is the SR2 builder (drift-
guarded against the DDL; a live idempotent + `byte_size`-refresh proof rides the
wamn-q3n.3 storage gate).

**Verification.** The artifact is validated **substrate-agnostically** (Q2): a
`WAMN_DUMP_PG_URL` round-trip gate (`crates/wamn-provision/tests/dump.rs`) seeds a
database, dumps it with the real `pg_dump_argv`, `pg_restore`s into a scratch DB,
and asserts the seed (incl an exact-decimal column) survives. The **in-cluster
gate of record** (the .6/.7/.9 precedent — debug binary + `kubectl`, no image
rebuild) provisions a real project-env database on the T3 pool `wamn-pg`, seeds it,
runs `dump-project-env --run-now` (reading the real `wamn-sysdb` registry),
`pg_restore`s into a scratch DB, asserts the round-trip, and checks the
`provisioning.dumps` row — teardown drops **only** the new database + scratch +
registry rows. Mutation-tested (apply/test/restore, debug builds): `pg_dump`
dropping `-Fd`, a wrong object-key shape, a CronJob command
without `-Fd`, and `record_dump` `ON CONFLICT DO UPDATE`→`DO NOTHING` each fail a
named test.

## `wamn-host restore-project-env` (wamn-q3n.11)

The **restore counterpart** of `dump-project-env`: `pg_restore` a `pg_dump -Fd`
artifact back into a database. This is the *logical-dump* restore path (docs/
postgres-topology.md §Backup architecture, restore runbook) — it restores from a
dump, not from a base backup. Whole-cluster **PITR** (rewind an org cluster to an
arbitrary instant, then carve one DB out) needs WAL/PITR and is wamn-e1g; this
subcommand is cross-referenced from that runbook, not a substitute for it.

The pure builder lives in `crates/wamn-provision/src/restore.rs` (the `dump.rs`
precedent — no clock, no DB, no `pg_restore` invocation):

- `pg_restore_argv(conninfo, dump_dir, clean)` — `--no-owner --no-privileges`
  (restore the **data**, not the source roles/ACLs — the target is owned by
  `wamn_app`, not the dump's roles), and, in place only, `--clean --if-exists`
  (**drop** each object before recreating it, so a restore over the live populated
  database replaces rather than appends);
- `restore_scratch_db_name(triple)` — `wamn-restore-<org>--<project>--<env>`, a
  name distinct from the live `wamn-db-…` so a scratch restore never shadows the
  real database (length-bounded by `validate_restore_scratch_name`).

`wamn-host restore-project-env` drives them:

```
restore-project-env --org <org> --project <p> --env <slug>
  --database-url <superuser to the TARGET cluster>   # create scratch + pg_restore
  [--system-database-url <sysdb>]        # read the dump catalog (restore-to-last-dump)
  [--dump-dir <-Fd dir> | --object-key <key>]  # explicit dump; else the latest
  [--dump-root <dir>]                    # local stage of the object store (until e1g)
  [--in-place --confirm]                 # destructive: pg_restore --clean over the LIVE db
```

Two **targets**, the safe one the default:

- **scratch** (default, non-destructive): restore into a fresh
  `wamn-restore-…` database so the dump can be inspected or a single table carved
  out without touching the live project-env DB — the sub-cluster carve-out path.
  The scratch DB is left standing for inspection (the drop command is printed);
- **in place** (`--in-place --confirm`, destructive): `pg_restore --clean` over the
  live project-env database — restore-to-last-dump. `--confirm` is **required**
  because it drops and replaces live data.

**Which dump (the catalog, Q3 of .10 realized here):** an explicit `--dump-dir`
wins; otherwise the dump **catalog** (`provisioning.dumps`) is read via
`wamn_registry::sql::select_latest_dump_sql` (SR2 builder) for the latest recorded
dump (or `--object-key`), so **restore-to-last-dump needs no manual key**. The dump
directory is then `--dump-root/<timestamp>` (the object key's last segment — the
`dump-project-env --run-now --out-dir` layout). The dump **bytes** are staged
locally until the shared object store lands (**Q2**, wamn-e1g); the catalog decides
*which* dump. `select_latest_dump_sql` orders `taken_at DESC, object_key DESC`
(newest first, tiebroken by the timestamp-suffixed key); `select_dumps_sql` lists
the window.

**Verification.** The restore is validated **substrate-agnostically**: a
`WAMN_RESTORE_PG_URL` round-trip gate (`crates/wamn-provision/tests/restore.rs`)
seeds a database, dumps it with the real `pg_dump_argv`, restores with the real
`pg_restore_argv` into a scratch DB, and asserts the seed (incl an exact-decimal
column) survives — then restores **in place** (`clean = true`) over a database
holding a stale row and asserts `--clean` dropped it (the restored state replaces,
not appends). A live `select_latest_dump_sql` proof (newest-of-three, taken_at +
object_key tiebreak) rides the wamn-q3n.3 storage gate. The **in-cluster gate of
record** (the .6/.7/.9/.10 precedent — debug binary + `kubectl`, no image rebuild)
provisions a real project-env database on the T3 pool `wamn-pg`, seeds + dumps it
(recording the real `wamn-sysdb` catalog), restores **to-last-dump** into a scratch
DB (round-trip asserted), and restores **in place** over the live DB after mutating
it (the stale row gone) — teardown drops **only** the new database + scratch +
registry rows (the dump record cascades). Mutation-tested (apply/test/restore,
debug builds): `pg_restore_argv` dropping `--clean` or `--no-owner`,
`select_latest` flipping `taken_at DESC`, and the in-place `--confirm` gate
neutered each fail a named test.

## `wamn-host move-org-tier` (wamn-q3n.13) — **retired** (wamn-8df.3)

`move-org-tier` (and its pure core `wamn_provision::tier_move`) shipped with the
closed `Tier` lattice: promote an org T3 → T2 → T4 by dump / provision-on-new /
restore / registry-flip, planned as an ordered runbook with the flip last. With
`Tier` dropped (D18), the subcommand and module are **removed** — a placement
change is no longer a privileged "tier upgrade" but one case of the **unified
`copy(src → dst)` operation** (`docs/deployment-model.md` §4, `wamn-8df.5`),
which reintroduces the move/cutover with a mandatory **quiesce + verify** gate
(fixing the cjv.7 dump→flip write-loss window) over arbitrary triples. Until
`.5` lands, a cross-cluster move is the manual runbook: `dump-project-env` →
`provision-org` (the new placement) → `provision-project-env` →
`restore-project-env` → update the org's placement row. **CNPG `initdb.import`**
(a `bootstrap.import` microservice import on the new `Database`/`Cluster`)
remains the documented CNPG-native alternative to the `.11` `pg_restore` path.

## Deferred (follow-up beads)

- **WAL archiving / PITR** — a fast-follow `[2.3]` bead (needs an in-cluster
  object store / MinIO). The restore **drill** stays 10.3 (wamn-tao). The MVP is
  provisioning + credentials + isolation only.
- **Live `K8sSecretProvider` read** — wamn-5x0.1 `[2.2b]`. 2.3 emits the Secret
  in the layout it reads.
- **Per-project distinct roles/passwords** — 8.2 hardening. The provisioning
  path leaves the `CREATE ROLE` seam for it (and for the dispatch role wamn-286
  and the user-SQL role wamn-1nd).
- **Migrating the legacy `deploy/postgres.yaml` fixtures onto CNPG** — a separate
  later bead; the MVP coexists.
- **Control-plane automation** of provisioning (a Project CRD + controller) —
  10.1.

## Build & test

See the `[2.3]` and `[D6/wamn-q3n.*]` blocks in `docs/build-and-test.md` for the
exact commands (pure crate unit + live-apply, the in-cluster `provisionbench`
gate of record, and the CNPG standups).

## References

- Plan: `docs/platform-plan.md` §2.3, D6.
- Credential seam: `crates/wamn-host/src/plugins/wamn_postgres.rs`
  (`CredentialProvider` / `StaticCredentialProvider` / `ProjectConfig`).
- D6 decision: `docs/platform-plan.md` decision table.
