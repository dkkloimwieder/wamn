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

## `wamn-host provision-org` (wamn-q3n.6, the four-tier split)

The four-tier topology (`docs/postgres-topology.md`) splits `provision-project`
into `provision-org` + `provision-project-env`. **`provision-org`** renders the
CNPG `Cluster` **pair** a paying org (T2 `standard` / T4 `dedicated`) is placed
on and records the org in the T1 control-plane registry:

- **`<org>-prod`** — HA (2 instances for `standard`, 3 for the regulated
  `dedicated` tier), pod anti-affinity spread, holds every project's `prod` env
  (and `canary`, which shares prod's failure domain);
- **`<org>-dev`** — a single, **hibernation-managed** instance (the
  `cnpg.io/hibernation` annotation, set `off` so it comes up ready; the platform
  off-hours scheduler flips it `on`, roughly halving the cost of two clusters per
  org), holds `dev` and preview/scratch envs (its own recovery domain).

Both clusters carry `enableSuperuserAccess` (the per-project-env path connects as
superuser), a non-TLS `pg_hba`, and **no cpu limit** (the S2 CFS lesson). The
cluster **names** come from `wamn_registry::cluster_name` — the single source the
renderer and the `registry.orgs` row share, so `Registry::resolve` finds the
provisioned cluster.

| Flag | Purpose |
|---|---|
| `--org <id>` | Org id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric). Names `<org>-prod` / `<org>-dev`; the reserved `wamn` prefix is rejected. |
| `--tier standard\|dedicated` | Paying tier. A `trials` org shares the pool (not a pair) and is not provisioned here — T3 provisioning is wamn-q3n.9. |
| `--system-database-url` / `WAMN_SYSTEM_ADMIN_URL` | Superuser URL to the T1 `wamn_system` DB, where the org row is recorded. Omit to render CRs only. |
| `--emit-prod` / `--emit-dev` `<path>` | Write the prod / dev `Cluster` CR (JSON; `-` = stdout, the default) — `kubectl apply -f`. |

Like `provision-project`, it is a **renderer + DB writer only** — it does NOT
apply the CRs (the runbook/Job `kubectl apply`s them and waits ready, as the
Secret pattern) and does NOT create per-project-env databases. The org row is an
**idempotent upsert** into `registry.orgs` (`ON CONFLICT (id) DO UPDATE`), written
as the `wamn_system` owner; the builder lives with the registry model
(`wamn_registry::sql::upsert_org_sql`, SR2 single-source).

**Scope (wamn-q3n.6): the cluster SHAPE + the registry row only.** Per-project-env
database/role creation is `provision-project-env` (wamn-q3n.7). The rework
**adopts the CNPG `Database` CRD + `.spec.managed.roles`** for that declarative
DB/role creation (keeping only the thin imperative `CONNECT`-revoke / `GRANT` /
RLS step CRDs don't cover; the CRD's `connectionLimit` doubles as per-project-env
noisy-neighbour governance) — the mechanism `.7` will use, but `.6` does not build
the unused renderer. Live **WAL/PITR backup** config (a `backup` stanza + an
object-store prefix) is `wamn-e1g` — the rendered clusters carry no `backup`
stanza yet.

The gate of record is a **live one-org-pair standup** (the wamn-q3n.2 infra
precedent): render + record via the real subcommand against the T1 `wamn-sysdb`,
`kubectl apply` the pair alongside the guardrailed clusters, wait ready, and
assert HA (a streaming replica + anti-affinity spread), the dev hibernation
annotation, the distinct plane (own Services / Secrets / PVCs), and that the
registry row's cluster names equal the live cluster names. Teardown deletes
**only** the new pair (never `wamn-pg` / `postgres.yaml` / `wamn-sysdb`).

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

See the `[2.3]` block in `CLAUDE.md` / `AGENTS.md` for the exact commands (pure
crate unit + live-apply, the in-cluster `provisionbench` gate of record, and the
CNPG standup).

## References

- Plan: `docs/platform-plan.md` §2.3, D6.
- Credential seam: `crates/wamn-host/src/plugins/wamn_postgres.rs`
  (`CredentialProvider` / `StaticCredentialProvider` / `ProjectConfig`).
- D6 decision: `docs/platform-plan.md` decision table.
