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
-- THE GENERIC DEPLOYMENT MODEL (D18, docs/deployment-model.md, wamn-8df.3):
-- the closed tier/env CHECK enumerations are RETIRED. `env` is a validated slug
-- resolving a named `registry.env_policies` row (referential integrity — the FK
-- below — replaces the old `env IN ('dev','canary','prod')` CHECK), and an org
-- carries a minimal placement (`pooled` | `dedicated`) from which clusters
-- derive (crates/wamn-registry cluster_of). The default env set (`dev`, `prod`)
-- is DATA seeded here, not a type; `canary` and others are addable policies.
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
--       and `canary` isolated (T4) is `recovery-domain: own` (→ <org>-canary). The
--       structural CHECK that remains is only `pooled ⟺ pool_cluster present`.

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
-- The only structural CHECK is `pooled ⟺ pool_cluster present`: a pooled org
-- names its shared pool; a dedicated org's clusters are derived, so pool is NULL.
-- Invariant 4 (dev ≠ prod recovery domain) is now the derivation's + validate()'s
-- job (header note), not a per-org CHECK.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.orgs (
    id             text PRIMARY KEY,
    placement_kind text NOT NULL,
    pool_cluster   text,
    CONSTRAINT orgs_placement_kind_check
        CHECK (placement_kind IN ('pooled', 'dedicated')),
    CONSTRAINT orgs_pool_cluster_check
        CHECK ((placement_kind = 'pooled') = (pool_cluster IS NOT NULL))
);

-- ---------------------------------------------------------------------------
-- Env policies (D18, wamn-8df.3) — named, self-contained environment
-- configurations (crates/wamn-registry EnvPolicy). The `name` IS the env slug: a
-- project-env's `env` both identifies it in the triple and (via the FK below)
-- resolves its policy. `recovery_domain` is `jsonb` (`"own"` | `{"shared-with":
-- "<env>"}`) driving the cluster derivation; the rest are the sizing / HA /
-- backup knobs `provision-org` renders each cluster from (fixing cjv.21). Standalone
-- (no inheritance) — the template layer that stamps them is wamn-8df.4.
--
-- SEEDED with the two defaults (`dev`, `prod`). These MUST match the model's
-- EnvPolicy::dev() / prod() (drift-guarded against crates/wamn-registry). `canary`
-- is NOT built in — it is added as a policy (shared-with prod = T2, own = T4).
-- ---------------------------------------------------------------------------
CREATE TABLE registry.env_policies (
    name            text PRIMARY KEY,
    recovery_domain jsonb NOT NULL,
    promotion_rank  int   NOT NULL,
    instances       int   NOT NULL,
    storage         text  NOT NULL,
    cpu             text  NOT NULL,
    memory          text  NOT NULL,
    image           text  NOT NULL,
    backup_cadence  text  NOT NULL DEFAULT '',
    wal_retention   text  NOT NULL DEFAULT '',
    hibernation     text  NOT NULL DEFAULT 'off'
);
INSERT INTO registry.env_policies
    (name, recovery_domain, promotion_rank, instances,
     storage, cpu, memory, image, backup_cadence, wal_retention, hibernation)
VALUES
    ('dev',  '"own"'::jsonb, 10, 1,
     '2Gi', '200m', '256Mi', 'ghcr.io/cloudnative-pg/postgresql:18', '',              '',    'eligible'),
    ('prod', '"own"'::jsonb, 30, 3,
     '2Gi', '200m', '256Mi', 'ghcr.io/cloudnative-pg/postgresql:18', '0 0 */6 * * *', '14d', 'off');

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
-- Secret. `env` is a validated slug; the FK to `registry.env_policies (name)`
-- enforces that it names a known policy (D18 — referential integrity replaces
-- the retired `env IN ('dev','canary','prod')` CHECK).
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
    env              text NOT NULL REFERENCES registry.env_policies (name),
    secret_name      text NOT NULL,
    secret_namespace text,
    PRIMARY KEY (org, project, env),
    FOREIGN KEY (org, project) REFERENCES registry.projects (org, id) ON DELETE CASCADE
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
