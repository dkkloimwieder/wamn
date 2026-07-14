-- The T1 control-plane REGISTRY storage schema (wamn-q3n.3). The tables that
-- PERSIST the identity/placement model defined by crates/wamn-registry — orgs,
-- projects, and the provisioned (org, project, env) databases — plus a minimal
-- provisioning-saga state table (exactly-once / resumable).
--
-- This is the schema that fills the EMPTY wamn_system DB the T1 cluster
-- (deploy/wamn-sysdb.yaml, wamn-q3n.2) bootstraps: registry-model → registry
-- tables, the way deploy/catalog-schema.sql followed crates/wamn-catalog.
--
-- STANDALONE ARTIFACT: deliberately NOT included by deploy/postgres-init.sql
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
--   (4) dev ≠ prod recovery domain — a DB CHECK on `orgs`: a paying org's
--       prod-side and dev-side clusters MUST differ; only the T3 trials pool
--       deliberately collapses both onto the shared cluster. Mirrors the model's
--       Env::side / resolve() routing; a rejected bad-standard-org proves it.

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
-- Orgs — the unit of isolation and billing. `tier` places the org
-- (trials/standard/dedicated); `prod_cluster` / `dev_cluster` are references (a
-- name) to the CNPG Clusters holding its prod-side and dev-side databases.
--
-- INVARIANT 4 (dev ≠ prod recovery domain): `orgs_recovery_domain_check`. A
-- paying org (standard/dedicated) MUST place prod and dev on different clusters
-- so a dev restore never rewinds prod; only the T3 trials pool collapses both
-- onto the shared cluster (canary shares prod's cluster by the env→side routing,
-- not by column). Mirrors crates/wamn-registry Env::side / resolve().
-- ---------------------------------------------------------------------------
CREATE TABLE registry.orgs (
    id            text PRIMARY KEY,
    tier          text NOT NULL,
    prod_cluster  text NOT NULL,
    dev_cluster   text NOT NULL,
    CONSTRAINT orgs_tier_check CHECK (tier IN ('trials', 'standard', 'dedicated')),
    CONSTRAINT orgs_recovery_domain_check
        CHECK (tier = 'trials' OR prod_cluster <> dev_cluster)
);

-- ---------------------------------------------------------------------------
-- Projects — structure within an org. Unique per org (the composite PK);
-- FK to the owning org.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.projects (
    org  text NOT NULL REFERENCES registry.orgs (id) ON DELETE CASCADE,
    id   text NOT NULL,
    PRIMARY KEY (org, id)
);

-- ---------------------------------------------------------------------------
-- Project-envs — the registry LEAF: one provisioned (org, project, env)
-- database, keyed by the identity triple, with a REFERENCE to its credential
-- Secret. `env` is the closed set dev/canary/prod.
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
    CONSTRAINT project_envs_env_check CHECK (env IN ('dev', 'canary', 'prod'))
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
        CHECK (kind IN ('provision-org', 'provision-project-env')),
    CONSTRAINT sagas_status_check
        CHECK (status IN ('pending', 'running', 'completed', 'failed',
                          'compensating', 'compensated')),
    CONSTRAINT sagas_step_nonneg CHECK (step >= 0)
);
