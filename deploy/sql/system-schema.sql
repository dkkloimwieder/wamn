-- The T1 control-plane REGISTRY storage schema (wamn-q3n.3). The tables that
-- PERSIST the identity/placement model defined by crates/wamn-registry — orgs,
-- projects, and the provisioned (org, project, env) databases — plus a minimal
-- provisioning-saga state table (exactly-once / resumable).
--
-- This is the schema that fills the EMPTY wamn_system DB the T1 cluster
-- (deploy/platform/wamn-sysdb.yaml, wamn-q3n.2) bootstraps: registry-model → registry
-- tables, the way deploy/sql/catalog-schema.sql followed crates/wamn-catalog.
--
-- STANDALONE ARTIFACT: deliberately NOT included by deploy/sql/postgres-init.sql
-- (which builds the S2–S6 *tenant-data* fixtures). This is applied into the T1
-- system DB — a different plane entirely.
--
-- PLATFORM-GLOBAL, NOT TENANT-SCOPED — the sharpest difference from
-- catalog-schema.sql: the system DB is the platform's own single-tenant
-- control-plane state. There is NO `app.tenant` claim, NO per-tenant RLS floor,
-- NO NULLIF/CHECK(tenant_id <> '') pattern here. The top-level key is `org_id`;
-- access is the `wamn_system` owner role (from wamn-q3n.2) plus a future
-- least-privilege control-plane role (the 8.1 RBAC seam — GRANT lines below).
--
-- APPLY AS THE `wamn_system` OWNER (it owns the system DB, so it can CREATE
-- SCHEMA): the registry is then owned by — and usable by — that role, which is
-- what .6 provision-org connects as. A superuser driving the apply `SET ROLE
-- wamn_system` first.
--
-- THE GENERIC DEPLOYMENT MODEL (D18, docs/deployment-model.md, wamn-8df.3;
-- org-scoped policies + templates, wamn-8df.4): the closed tier/env CHECK
-- enumerations are RETIRED. `env` is a validated slug resolving a
-- `registry.env_policies` row IN ITS ORG's set (referential integrity — the
-- composite FK below — replaces the old `env IN ('dev','canary','prod')`
-- CHECK), and an org carries a minimal placement (`pooled` | `dedicated`) from
-- which clusters derive (crates/wamn-registry cluster_of). Policies are
-- PER-ORG rows stamped from a named Template preset (`trials` / `standard` /
-- `dedicated` — the Tier successor, crates/wamn-registry Template) at
-- provision-org time; there is NO platform-global policy seed — an org
-- instantiates a template and then customizes its own rows per-env.
--
-- THE FOUR INVARIANTS (docs/postgres-topology.md §T1), and how this schema
-- encodes / makes each testable:
--   (1) request-path-free  — an ARCHITECTURAL property, not a DB constraint. No
--       data-plane workload (gateway/runner/dispatcher/webhook) may reference
--       this cluster or DB; only control-plane tooling connects here. A static
--       manifest grep (crates/wamn-registry/tests/storage.rs) guards it.
--   (2) no credentials (R8b) — `project_envs` stores a Secret *reference*
--       (secret_name + optional secret_namespace) and NO credential column
--       (no url/password/dsn). Asserted by the drift-guard + the live-apply gate.
--   (3) no tenant data — the only tables here are the control-plane set below
--       (registry + provisioning). No catalogs, run state, payloads, or
--       application users. The live-apply gate asserts the exact table set.
--   (4) dev ≠ prod recovery domain — under D18 this is enforced by the DERIVATION
--       plus crates/wamn-registry validate(), not a per-org DB CHECK: two
--       own-domain envs (dev, prod) derive distinct clusters (<org>-dev vs
--       <org>-prod) by construction, so they never collapse; `canary` collapsing
--       onto prod is an INTENTIONAL `recovery-domain: {shared-with: prod}` policy,
--       and `canary` isolated (T4) is `recovery-domain: own` (→ <org>-canary).
--
-- id/name WELL-FORMEDNESS: crates/wamn-registry validate() (check_id/check_env/
-- check_name) is the PRIMARY guard, and it runs on the in-memory `from_json`
-- import path a direct control-plane writer (e.g. wamn-2ib) uses. But a writer
-- that skips BOTH provision-org AND Registry::validate() would land a malformed
-- id straight into K8s object names + WAL paths with no guard, so the stored
-- slug/name columns also carry a defensive charset/length CHECK backstop that
-- mirrors validate() (wamn-cjv.20). The structural CHECKs on `orgs` are thus
-- `pooled ⟺ pool_cluster present` plus that charset/length backstop.

-- ---------------------------------------------------------------------------
-- Schemas. `registry` = the identity/placement model (wamn-q3n.1); `provisioning`
-- = the saga state that orchestrates it (10.1's exactly-once/resumable steps).
-- Distinct schemas so each control-plane subsystem is namespaced, and the
-- no-tenant-data table set (invariant 3) is exactly what these two hold.
-- Owned by the `wamn_system` role the T1 cluster bootstraps (wamn-q3n.2).
-- ---------------------------------------------------------------------------
CREATE SCHEMA registry AUTHORIZATION wamn_system;
CREATE SCHEMA provisioning AUTHORIZATION wamn_system;

-- RBAC seam (8.1): a future least-privilege control-plane role (builder/admin/
-- viewer, distinct from the tenant `wamn_app`) is GRANTed here. The `wamn_system`
-- owner needs no grant. Left as a documented seam — RBAC is a separate subsystem.
--   GRANT USAGE ON SCHEMA registry, provisioning TO wamn_control;
--   GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA registry TO wamn_control;

-- ---------------------------------------------------------------------------
-- Registry format version (singleton). Records the storage-format version,
-- aligned with crates/wamn-registry SCHEMA_VERSION; additive-within-major per the
-- 0.1.x freeze. A single-row table (the `id` boolean PK + CHECK forbids a second
-- row) — the registry is not a versioned per-row document like the catalog.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.meta (
    id             boolean PRIMARY KEY DEFAULT true CHECK (id),
    schema_version text NOT NULL
);
INSERT INTO registry.meta (schema_version) VALUES ('0.1');

-- ---------------------------------------------------------------------------
-- Orgs — the unit of isolation and billing. Carries only the id and a minimal
-- D18 PLACEMENT (crates/wamn-registry Placement): `placement_kind` is `pooled`
-- (every env shares `pool_cluster`, the T3-style pool) or `dedicated` (the org
-- owns one cluster per recovery domain, `<org>-<owner(env)>`, DERIVED by
-- cluster_of — not stored). The retired `tier` / `prod_cluster` / `canary_cluster`
-- / `dev_cluster` columns are gone; sizing/HA/backup are env-policy knobs.
--
-- Structural CHECKs: `pooled ⟺ pool_cluster present` (a pooled org names its
-- shared pool; a dedicated org's clusters are derived, so pool is NULL) plus a
-- defensive charset/length backstop (cjv.20) on `id` and `pool_cluster` mirroring
-- crates/wamn-registry validate() (check_id / check_name) — `id` must be a
-- lowercase slug `[a-z0-9-]` (start/end alnum), ≤ 40 bytes, and not under the
-- reserved `wamn` prefix (it mints `<org>-*` cluster / `wamn-db-<org>--*` Secret /
-- subdomain names); `pool_cluster` is a DNS-1123 label ≤ 63 and MAY carry the
-- `wamn` prefix (`wamn-pg`), so no reserved rule there. Invariant 4 (dev ≠ prod
-- recovery domain) is still the derivation's + validate()'s job, not a CHECK.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.orgs (
    id             text PRIMARY KEY,
    placement_kind text NOT NULL,
    pool_cluster   text,
    CONSTRAINT orgs_placement_kind_check
        CHECK (placement_kind IN ('pooled', 'dedicated')),
    CONSTRAINT orgs_pool_cluster_check
        CHECK ((placement_kind = 'pooled') = (pool_cluster IS NOT NULL)),
    CONSTRAINT orgs_id_charset_check
        CHECK (id ~ '^[a-z0-9]([a-z0-9-]*[a-z0-9])?$'
               AND char_length(id) <= 40
               AND id <> 'wamn' AND id NOT LIKE 'wamn-%'),
    CONSTRAINT orgs_pool_cluster_charset_check
        CHECK (pool_cluster IS NULL
               OR (pool_cluster ~ '^[a-z0-9]([a-z0-9-]*[a-z0-9])?$'
                   AND char_length(pool_cluster) <= 63))
);

-- ---------------------------------------------------------------------------
-- Env policies (D18, wamn-8df.3; ORG-SCOPED by wamn-8df.4) — each org's own
-- environment configurations (crates/wamn-registry EnvPolicy, keyed per org as
-- OrgEnvPolicy). The `name` IS the env slug: a project-env's `env` both
-- identifies it in the triple and (via the composite FK below) resolves its
-- policy in ITS ORG's set. `recovery_domain` is `jsonb` (`"own"` |
-- `{"shared-with": "<env>"}`) driving the cluster derivation; the rest are the
-- sizing / HA / backup knobs `provision-org` renders each cluster from.
--
-- NO PLATFORM-GLOBAL SEED: rows are STAMPED per org from a Template preset
-- (`trials` / `standard` / `dedicated`) by provision-org — insert-if-absent
-- (stamp_env_policy_sql), so an org's per-env customizations survive
-- re-provisioning and a template edit never silently resizes an existing
-- customer. Org-scoping is what lets a T2 org (canary shared-with prod) and a
-- T4 org (canary own) coexist on one platform. Cascades with its org.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.env_policies (
    -- the org FK (CASCADE) is added AFTER project_envs below — ordering note there
    org             text NOT NULL,
    name            text NOT NULL,
    recovery_domain jsonb NOT NULL,
    promotion_rank  int   NOT NULL,
    instances       int   NOT NULL,
    storage         text  NOT NULL,
    cpu             text  NOT NULL,
    memory          text  NOT NULL,
    image           text  NOT NULL,
    backup_cadence  text  NOT NULL DEFAULT '',
    wal_retention   text  NOT NULL DEFAULT '',
    hibernation     text  NOT NULL DEFAULT 'off',
    PRIMARY KEY (org, name),
    -- cjv.20 charset backstop: `name` IS the env slug (check_env mirror) — a
    -- lowercase slug ≤ 40 bytes; no reserved rule (an env may be any slug).
    CONSTRAINT env_policies_name_charset_check
        CHECK (name ~ '^[a-z0-9]([a-z0-9-]*[a-z0-9])?$'
               AND char_length(name) <= 40)
);

-- ---------------------------------------------------------------------------
-- Projects — structure within an org. Unique per org (the composite PK);
-- FK to the owning org.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.projects (
    org  text NOT NULL REFERENCES registry.orgs (id) ON DELETE CASCADE,
    id   text NOT NULL,
    PRIMARY KEY (org, id),
    -- cjv.20 charset backstop: project `id` is a check_id mirror (lowercase slug
    -- ≤ 40 bytes, not under the reserved `wamn` prefix — it too mints names).
    CONSTRAINT projects_id_charset_check
        CHECK (id ~ '^[a-z0-9]([a-z0-9-]*[a-z0-9])?$'
               AND char_length(id) <= 40
               AND id <> 'wamn' AND id NOT LIKE 'wamn-%')
);

-- ---------------------------------------------------------------------------
-- Project-envs — the registry LEAF: one provisioned (org, project, env)
-- database, keyed by the identity triple, with a REFERENCE to its credential
-- Secret. `env` is a validated slug; the composite FK to
-- `registry.env_policies (org, name)` enforces that it names a policy in ITS
-- ORG's set (D18 + 8df.4 — referential integrity replaces the retired
-- `env IN ('dev','canary','prod')` CHECK; another org's policy never
-- satisfies it). The policy FK is deliberately NOT a CASCADE: a policy in use
-- by a provisioned env cannot be dropped (a cascade would silently erase the
-- record of a real provisioned database). It is DEFERRABLE INITIALLY IMMEDIATE
-- — inserts are checked immediately; `SET CONSTRAINTS ... DEFERRED` inside a
-- transaction is the order-independent escape hatch for bulk teardown (the
-- env_policies org-FK ordering note below makes plain single-statement org
-- DELETEs cascade cleanly on a fresh apply).
--
-- INVARIANT 2 (no credentials, R8b): `secret_name` (+ optional
-- `secret_namespace`) is a REFERENCE — the actual credential material lives in a
-- K8s Secret resolved by a component holding the matching RBAC. There is NO
-- url/password/dsn column here; compromise of this DB yields the org/placement
-- *list*, never the keys to any org's data.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.project_envs (
    org              text NOT NULL,
    project          text NOT NULL,
    env              text NOT NULL,
    secret_name      text NOT NULL,
    secret_namespace text,
    PRIMARY KEY (org, project, env),
    FOREIGN KEY (org, project) REFERENCES registry.projects (org, id) ON DELETE CASCADE,
    FOREIGN KEY (org, env) REFERENCES registry.env_policies (org, name)
        DEFERRABLE INITIALLY IMMEDIATE
);

-- The env_policies → orgs CASCADE is added HERE, after projects/project_envs
-- exist: Postgres fires an org DELETE's cascade triggers in creation order, so
-- adding this FK LAST means the projects → project_envs cascade clears the
-- referencing rows BEFORE the policy rows are deleted — a plain
-- `DELETE FROM registry.orgs WHERE id = ...` tears a whole org down in one
-- statement without tripping the in-use-policy FK above. (If a dump/restore
-- ever reorders the triggers, the DEFERRABLE escape hatch above still works.)
ALTER TABLE registry.env_policies
    ADD CONSTRAINT env_policies_org_fkey
    FOREIGN KEY (org) REFERENCES registry.orgs (id) ON DELETE CASCADE;

-- ---------------------------------------------------------------------------
-- Event readers — the CDC capture registrations (D19 v3, wamn-l5i9.9). One row
-- per project-env with CDC enabled: the publication + failover replication slot
-- the reader streams from (the `wamn_cdc_…` Postgres objects the
-- enable-cdc-project-env overlay provisions), the JetStream stream envelopes
-- land in (`EVT_<org>_<env>` by default), and a REFERENCE to the reader's
-- replication-credential Secret (`wamn-cdc-<org>--<project>--<env>`).
--
-- INVARIANT 2 (no credentials, R8b): `replication_secret_name` (+ optional
-- namespace) is a reference only — the replication credential is its own tier
-- ABOVE the wamn_app query credential, and its material lives in a K8s Secret,
-- never here. Keyed by the identity triple; FK to the project-env it captures
-- (the provisioning.dumps precedent), so a de-provisioned env (or a deleted
-- org, cascading through project_envs) drops its registration. The reader
-- service (l5i9.10) reads its row to learn what to stream.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.event_readers (
    org                          text NOT NULL,
    project                      text NOT NULL,
    env                          text NOT NULL,
    publication                  text NOT NULL,
    slot                         text NOT NULL,
    stream                       text NOT NULL,
    replication_secret_name      text NOT NULL,
    replication_secret_namespace text,
    enabled                      boolean NOT NULL DEFAULT true,
    created_at                   timestamptz NOT NULL DEFAULT now(),
    updated_at                   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (org, project, env),
    FOREIGN KEY (org, project, env)
        REFERENCES registry.project_envs (org, project, env) ON DELETE CASCADE
);

-- ---------------------------------------------------------------------------
-- Provisioning sagas — minimal exactly-once / resumable state for the
-- provisioning orchestrator (10.1; consumed by .6 provision-org / .7
-- provision-project-env). One row per saga run.
--
-- `target` is decoupled text (the org id, or the `org/project/env` triple) — NOT
-- an FK, because a provision-org saga runs BEFORE its org row exists (it creates
-- it). `step` is the durable resume checkpoint (the write-ahead pattern this repo
-- uses for exactly-once: the orchestrator advances `step` in the SAME txn as each
-- step's effect, so a crash-then-resume re-reads `step` and never re-applies a
-- committed step); creating a saga is exactly-once via the `saga_id` PK
-- (INSERT … ON CONFLICT (saga_id) DO NOTHING). The per-step compensation LEDGER
-- (rollback in reverse) is 10.1's to elaborate on top of this row.
-- ---------------------------------------------------------------------------
CREATE TABLE provisioning.sagas (
    saga_id     text PRIMARY KEY,
    kind        text NOT NULL,
    target      text NOT NULL,
    status      text NOT NULL DEFAULT 'pending',
    step        int  NOT NULL DEFAULT 0,
    total_steps int,
    last_error  text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT sagas_kind_check
        CHECK (kind IN ('provision-org', 'provision-project-env', 'copy')),
    CONSTRAINT sagas_status_check
        CHECK (status IN ('pending', 'running', 'completed', 'failed',
                          'compensating', 'compensated')),
    CONSTRAINT sagas_step_nonneg CHECK (step >= 0)
);

-- ---------------------------------------------------------------------------
-- Dumps — bookkeeping for the scheduled per-project-env LOGICAL DUMPS
-- (wamn-q3n.10, the second backup mechanism; docs/postgres-topology.md §Backup
-- architecture). One row per dump taken: the object-store `object_key`
-- (`dumps/<org>/<project>/<env>/<timestamp>` — derivable, this row is a record
-- not the source), the dump `format` (`pg_dump -Fd` = directory), the completed
-- `byte_size`, and when it was `taken_at`.
--
-- This is control-plane METADATA, not tenant data (invariant 3): no dump BYTES
-- (those live in object storage), no credentials (invariant 2). Keyed by the
-- (org, project, env) triple + object_key; FK to the project-env it dumps, so a
-- de-provisioned project-env (or a deleted org, cascading through project_envs)
-- drops its dump records. The dump CATALOG for RESTORE (find the latest dump)
-- is wamn-q3n.11's restore tooling; .10 only RECORDS what it produces.
-- ---------------------------------------------------------------------------
CREATE TABLE provisioning.dumps (
    org         text NOT NULL,
    project     text NOT NULL,
    env         text NOT NULL,
    object_key  text NOT NULL,
    format      text NOT NULL DEFAULT 'directory',
    byte_size   bigint,
    taken_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (org, project, env, object_key),
    FOREIGN KEY (org, project, env)
        REFERENCES registry.project_envs (org, project, env) ON DELETE CASCADE,
    CONSTRAINT dumps_format_check CHECK (format IN ('directory'))
);
