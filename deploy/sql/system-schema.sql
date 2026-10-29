-- The T1 control-plane REGISTRY storage schema (wamn-q3n.3). The tables that
-- PERSIST the identity/placement model defined by crates/control/registry — orgs,
-- projects, and the provisioned (org, project, env) databases.
--
-- This is the schema that fills the EMPTY wamn_system DB the T1 cluster
-- (deploy/platform/wamn-sysdb.yaml, wamn-q3n.2) bootstraps: registry-model → registry
-- tables. Project databases independently install the package/effective-release
-- catalog from deploy/sql/catalog-schema.sql.
--
-- STANDALONE ARTIFACT: deliberately NOT included by deploy/sql/postgres-init.sql
-- (which builds the S2–S6 *tenant-data* fixtures). This is applied into the T1
-- system DB — a different plane entirely.
--
-- PLATFORM-GLOBAL, NOT TENANT-SCOPED — the sharpest difference from
-- catalog-schema.sql: the system DB is the platform's own single-tenant
-- control-plane state. There is NO `app.env_key` claim, NO per-tenant RLS floor,
-- NO NULLIF/CHECK(tenant_id <> '') pattern here. The top-level key is `org_id`;
-- access is the `wamn_system` owner role (from wamn-q3n.2). The control-plane
-- families get their grants from `stable_surface_sql` in wamn-control-provision.
--
-- APPLY AS THE `wamn_system` OWNER (it owns the system DB, so it can CREATE
-- SCHEMA): the registry is then owned by — and usable by — that role, which is
-- what .6 provision-org connects as. A superuser driving the apply `SET ROLE
-- wamn_system` first.
--
-- RECORD HISTORY: stamped registry and identity relations carry created_at,
-- created_by, updated_at, and updated_by as NOT NULL columns, and a
-- wamn_record_history_stamp trigger that calls wamn_history.stamp_row(). An applier
-- installs deploy/sql/record-history.sql first. The SYSTEM_SCHEMA_SQL
-- composition in wamn-control-provision carries it. That file grants to
-- wamn_db_owner, so an applier that runs as wamn_system creates that role
-- first. Every write to these relations binds app.user_id: the identity issuer
-- and wamn-ctl bind wamn:provisioning, and test fixtures bind a principal.
--
-- THE GENERIC DEPLOYMENT MODEL (D18, wamn-8df.3;
-- org-scoped policies + templates, wamn-8df.4): the closed tier/env CHECK
-- enumerations are RETIRED. `env` is a validated slug resolving a
-- `registry.env_policies` row IN ITS ORG's set (referential integrity — the
-- composite FK below — replaces the old `env IN ('dev','canary','prod')`
-- CHECK), and an org carries a minimal placement (`pooled` | `dedicated`) from
-- which clusters derive (crates/control/registry cluster_of). Policies are
-- PER-ORG rows stamped from a named Template preset (`trials` / `standard` /
-- `dedicated` — the Tier successor, crates/control/registry Template) at
-- provision-org time; there is NO platform-global policy seed — an org
-- instantiates a template and then customizes its own rows per-env.
--
-- THE FOUR INVARIANTS for T1, and how this schema
-- encodes / makes each testable:
--   (1) control-plane authority: tenant workloads cannot connect here.
--       Scoped identity readers perform fresh PAT authentication.
--       The separate identity issuer has explicit key, credential, and throttle grants.
--   (2) no tenant-database credentials (R8b) — `project_envs` stores a Secret
--       *reference* (secret_name + optional secret_namespace) and NO tenant DB
--       credential column (no url/password/dsn). First-party user login hashes
--       live separately under `identity`. Session signing keys are the explicit
--       authority-only exception: only wamn-identity receives their private part.
--   (3) no tenant data — the only tables here are the control-plane set below
--       (registry + provisioning + first-party platform identity). No catalogs,
--       run state, payloads, or per-project application users. The live-apply
--       gate asserts the exact table set.
--   (4) dev ≠ prod recovery domain — under D18 this is enforced by the DERIVATION
--       plus crates/control/registry validate(), not a per-org DB CHECK: two
--       own-domain envs (dev, prod) derive distinct clusters (<org>-dev vs
--       <org>-prod) by construction, so they never collapse; `canary` collapsing
--       onto prod is an INTENTIONAL `recovery-domain: {shared-with: prod}` policy,
--       and `canary` isolated (T4) is `recovery-domain: own` (→ <org>-canary).
--
-- id/name WELL-FORMEDNESS: crates/control/registry validate() (check_id/check_env/
-- check_name) is the PRIMARY guard, and it runs on the in-memory `from_json`
-- import path a direct control-plane writer (e.g. wamn-2ib) uses. But a writer
-- that skips BOTH provision-org AND Registry::validate() would land a malformed
-- id straight into K8s object names + WAL paths with no guard, so the stored
-- slug/name columns also carry a defensive charset/length CHECK backstop that
-- mirrors validate() (wamn-cjv.20). The structural CHECKs on `orgs` are thus
-- `pooled ⟺ pool_cluster present` plus that charset/length backstop.

-- ---------------------------------------------------------------------------
-- Schemas. `registry` = the org/project placement model (wamn-q3n.1);
-- `provisioning` = operations state, empty today; `identity` = first-party platform principals,
-- local user credential hashes, project-role assignments (wamn-ctc8.6), and
-- personal-access-token digests (wamn-ctc8.7), and signing generations (wamn-ctc8.15.1).
-- Distinct schemas keep each control-plane subsystem namespaced.
-- Owned by the `wamn_system` role the T1 cluster bootstraps (wamn-q3n.2).
-- ---------------------------------------------------------------------------
CREATE SCHEMA registry AUTHORIZATION wamn_system;
CREATE SCHEMA provisioning AUTHORIZATION wamn_system;
CREATE SCHEMA identity AUTHORIZATION wamn_system;

-- Platform integration catalog and deployment targets.
CREATE TABLE registry.integration_type (
    code      text PRIMARY KEY CHECK (code ~ '^[a-z][a-z0-9_-]*$'),
    runtime_type text NOT NULL CHECK (runtime_type IN ('host', 'wasm', 'native')),
    exclusive boolean NOT NULL,
    resource_key_template text CHECK (resource_key_template <> ''),
    CHECK (exclusive = (resource_key_template IS NOT NULL))
);
CREATE TABLE registry.integration_type_version (
    integration_type text NOT NULL REFERENCES registry.integration_type (code),
    version          integer NOT NULL CHECK (version > 0),
    digest           text CHECK (digest ~ '^sha256:[0-9a-f]{64}$'),
    parameter_schema jsonb NOT NULL,
    operations       text NOT NULL,
    published_at     timestamptz NOT NULL,
    PRIMARY KEY (integration_type, version)
);
CREATE TABLE registry.auth_type (
    code              text PRIMARY KEY,
    credential_schema jsonb NOT NULL,
    parameter_schema  jsonb NOT NULL,
    http_only         boolean NOT NULL
);
CREATE TABLE registry.integration_type_auth (
    integration_type text NOT NULL REFERENCES registry.integration_type (code),
    auth_type        text NOT NULL REFERENCES registry.auth_type (code),
    PRIMARY KEY (integration_type, auth_type)
);

INSERT INTO registry.integration_type (code, runtime_type, exclusive) VALUES
    ('http', 'host', false), ('gcs', 'host', false), ('s3', 'host', false), ('modbus', 'wasm', false);

INSERT INTO registry.integration_type_version
    (integration_type, version, digest, parameter_schema, operations, published_at) VALUES
    ('http', 1, NULL,
     '{"type":"object","properties":{"primary-authority":{"type":"string","minLength":1,"immutable":true},"tls-verification":{"type":"string","enum":["disabled","verify-authority"]}},"required":["primary-authority","tls-verification"]}', 'wamn:connection/http', transaction_timestamp()),
    ('gcs', 1, NULL,
     '{"type":"object","properties":{"provider":{"type":"string","enum":["gcs"],"immutable":true},"container":{"type":"string","minLength":1,"immutable":true},"prefix":{"type":"string","minLength":1,"immutable":true},"cdn_host":{"type":"string","minLength":1}},"required":["container","prefix","provider"]}', 'wasmcloud:blobstore/imports@0.1.0', transaction_timestamp()),
    ('s3', 1, NULL,
     '{"type":"object","properties":{"provider":{"type":"string","enum":["s3"],"immutable":true},"container":{"type":"string","minLength":1,"immutable":true},"prefix":{"type":"string","minLength":1,"immutable":true},"cdn_host":{"type":"string","minLength":1},"endpoint":{"type":"string","minLength":1,"immutable":true}},"required":["container","prefix","endpoint"]}', 'wasmcloud:blobstore/imports@0.1.0', transaction_timestamp()),
    ('modbus', 1, 'sha256:23dc5ca1d9bbbb546ed4f92ca1c358ad04abbe0655f4c1afa76ff948ab5e08e4',
     '{"type":"object","properties":{"authority":{"type":"string","minLength":1,"immutable":true}},"required":["authority"]}', 'wamn:modbus/modbus', transaction_timestamp());

INSERT INTO registry.auth_type (code, credential_schema, parameter_schema, http_only) VALUES
    ('http_basic', '{"type":"object","properties":{"username":{"type":"string"},"password":{"type":"string"}},"required":["username","password"]}',
     '{"type":"object","properties":{},"additionalProperties":false}', true),
    ('api_key', '{"type":"object","properties":{"key":{"type":"string"}},"required":["key"]}',
     '{"type":"object","properties":{"in":{"type":"string","enum":["header","query"]},"name":{"type":"string"}},"required":["in","name"]}', true),
    ('oauth2_client_credentials', '{"type":"object","properties":{"client_secret":{"type":"string"}},"required":["client_secret"]}',
     '{"type":"object","properties":{"client_id":{"type":"string"},"token_url":{"type":"string"},"scopes":{"type":"array","items":{"type":"string"}}},"required":["client_id","token_url","scopes"]}', true),
    ('oauth2_authorization_code', '{"type":"object","properties":{"client_secret":{"type":"string"}},"required":["client_secret"]}',
     '{"type":"object","properties":{"client_id":{"type":"string"},"token_url":{"type":"string"},"scopes":{"type":"array","items":{"type":"string"}}},"required":["client_id","token_url","scopes"]}', true),
    ('aws_sigv4', '{"type":"object","properties":{"access_key_id":{"type":"string"},"secret_access_key":{"type":"string"}},"required":["access_key_id","secret_access_key"]}',
     '{"type":"object","properties":{"region":{"type":"string"},"service":{"type":"string"}},"required":["region","service"]}', true),
    ('custom', '{"type":"object"}',
     '{"type":"object","properties":{},"additionalProperties":false}', false),
    ('none', '{"type":"object","properties":{},"additionalProperties":false}',
     '{"type":"object","properties":{},"additionalProperties":false}', false);

INSERT INTO registry.integration_type_auth (integration_type, auth_type) VALUES
    ('http', 'http_basic'),
    ('http', 'api_key'),
    ('http', 'oauth2_client_credentials'),
    ('http', 'oauth2_authorization_code'),
    ('http', 'aws_sigv4'),
    ('http', 'none'),
    ('gcs', 'custom'),
    ('s3', 'custom'),
    ('modbus', 'none');

CREATE TABLE registry.target (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name text NOT NULL UNIQUE,
    type text NOT NULL CHECK (type IN ('local', 'cloud')),
    status text NOT NULL,
    last_seen_at timestamptz
);
INSERT INTO registry.target (name, type, status) VALUES ('cloud', 'cloud', 'active');


-- ---------------------------------------------------------------------------
-- Registry format version (singleton). Records the storage-format version,
-- aligned with crates/control/registry SCHEMA_VERSION; additive-within-major per the
-- 0.1.x freeze. A single-row table (the `id` boolean PK + CHECK forbids a second
-- row) — the registry is not a versioned per-row document like the catalog.
--
-- `platform_domain` is the deployment's platform principal email domain: every
-- tenant's `platform."user"` platform row carries `<component>@<domain>`
-- (wamn-0h0g.9.15). It belongs to the deployment and not to an org, so it sits
-- on this singleton. It starts NULL because static SQL cannot read deployment
-- configuration; the bootstrap that applies this file sets it, and
-- `reconcile-run-plane` refuses to create a tenant's platform rows while it is
-- unset. The CHECK carries the same shape and the same two length limits that
-- `wamn_control_provision::validate_platform_domain` enforces: dot-separated
-- labels of lowercase letters, digits and inner hyphens, each label at most 63
-- bytes and the whole name at most 253.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.meta (
    id              boolean PRIMARY KEY DEFAULT true CHECK (id),
    schema_version  text NOT NULL,
    platform_domain text,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    CONSTRAINT meta_platform_domain_check
        CHECK (platform_domain IS NULL
               OR (octet_length(platform_domain) <= 253
                   AND platform_domain ~ '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$'))
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON registry.meta
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');
-- The deployment metadata seed binds the platform actor in its transaction.
DO $meta$ BEGIN
  PERFORM pg_catalog.set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', true);
  INSERT INTO registry.meta (schema_version) VALUES ('0.1');
END $meta$;

-- ---------------------------------------------------------------------------
-- Orgs — the unit of isolation and billing. Carries only the id and a minimal
-- D18 PLACEMENT (crates/control/registry Placement): `placement_type` is `pooled`
-- (every env shares `pool_cluster`, the T3-style pool) or `dedicated` (the org
-- owns one cluster per recovery domain, `<org>-<owner(env)>`, DERIVED by
-- cluster_of — not stored). The retired `tier` / `prod_cluster` / `canary_cluster`
-- / `dev_cluster` columns are gone; sizing/HA/backup are env-policy knobs.
--
-- Structural CHECKs: `pooled ⟺ pool_cluster present` (a pooled org names its
-- shared pool; a dedicated org's clusters are derived, so pool is NULL) plus a
-- defensive charset/length backstop (cjv.20) on `id` and `pool_cluster` mirroring
-- crates/control/registry validate() (check_id / check_name) — `id` must be a
-- lowercase slug `[a-z0-9-]` (start/end alnum, NO consecutive hyphens — wamn-R27,
-- since `--` is the `wamn-db-<org>--<project>--<env>` component separator, so a
-- `--` run would let two triples collide onto one name), ≤ 40 bytes, and not under
-- the reserved `wamn` prefix (it mints `<org>-*` cluster / `wamn-db-<org>--*` Secret /
-- subdomain names); `pool_cluster` is a DNS-1123 label ≤ 63 and MAY carry the
-- `wamn` prefix (`wamn-pg`), so no reserved rule there. Invariant 4 (dev ≠ prod
-- recovery domain) is still the derivation's + validate()'s job, not a CHECK.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.orgs (
    id             text PRIMARY KEY,
    placement_type text NOT NULL,
    pool_cluster   text,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    CONSTRAINT orgs_placement_type_check
        CHECK (placement_type IN ('pooled', 'dedicated')),
    CONSTRAINT orgs_pool_cluster_check
        CHECK ((placement_type = 'pooled') = (pool_cluster IS NOT NULL)),
    CONSTRAINT orgs_id_charset_check
        CHECK (id ~ '^[a-z0-9]+(-[a-z0-9]+)*$'
               AND char_length(id) <= 40
               AND id <> 'wamn' AND id NOT LIKE 'wamn-%'),
    CONSTRAINT orgs_pool_cluster_charset_check
        CHECK (pool_cluster IS NULL
               OR (pool_cluster ~ '^[a-z0-9]([a-z0-9-]*[a-z0-9])?$'
                   AND char_length(pool_cluster) <= 63))
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON registry.orgs
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- ---------------------------------------------------------------------------
-- Env policies (D18, wamn-8df.3; ORG-SCOPED by wamn-8df.4) — each org's own
-- environment configurations (crates/control/registry EnvPolicy, keyed per org as
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
    durability_class text NOT NULL DEFAULT 'standard'
        CONSTRAINT env_policies_durability_class_check
        CHECK (durability_class IN ('standard')),
    -- The readiness budget and drain bound of `env apply`
    -- (docs/plan/platform-deploy.md §10.1).
    readiness_budget_seconds int NOT NULL DEFAULT 600
        CONSTRAINT env_policies_readiness_budget_check CHECK (readiness_budget_seconds > 0),
    drain_bound_seconds int NOT NULL DEFAULT 300
        CONSTRAINT env_policies_drain_bound_check CHECK (drain_bound_seconds > 0),
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (org, name),
    -- cjv.20 charset backstop: `name` IS the env slug (check_env mirror) — a
    -- lowercase slug ≤ 40 bytes; no reserved rule (an env may be any slug).
    CONSTRAINT env_policies_name_charset_check
        CHECK (name ~ '^[a-z0-9]+(-[a-z0-9]+)*$'
               AND char_length(name) <= 40)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON registry.env_policies
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- ---------------------------------------------------------------------------
-- Projects — structure within an org. Unique per org (the composite PK);
-- FK to the owning org.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.projects (
    org  text NOT NULL REFERENCES registry.orgs (id) ON DELETE CASCADE,
    id   text NOT NULL,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (org, id),
    -- cjv.20 charset backstop: project `id` is a check_id mirror (lowercase slug
    -- ≤ 40 bytes, not under the reserved `wamn` prefix — it too mints names).
    CONSTRAINT projects_id_charset_check
        CHECK (id ~ '^[a-z0-9]+(-[a-z0-9]+)*$'
               AND char_length(id) <= 40
               AND id <> 'wamn' AND id NOT LIKE 'wamn-%')
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON registry.projects
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- ---------------------------------------------------------------------------
-- First-party platform identity (wamn-ctc8.6). This is platform-plane
-- authentication state, distinct from per-project `platform` identities.
-- Users and services are passwordless subjects. PATs are the MVP presenter;
-- an external OIDC adapter may resolve a user subject through this same core.
-- Role slugs are opaque: permission meaning belongs to the management
-- authorization boundary.
--
-- `type` is user, service, or platform. A platform row names a platform
-- component that writes in this database, and only wamn:provisioning does. The
-- row carries its `wamn:<component>` name in `subject` and in `display_name`,
-- and the id that `wamn-project-state` derives from that name.
-- `principals_platform_principal_check` pins the subject, display name, and id
-- of each platform row, and it refuses a `wamn:` display name on another type.
-- The subject pattern refuses a colon on another type. PostgreSQL has no
-- UUIDv5, so a Rust test compares the literal with the derivation. A platform
-- principal cannot authenticate.
--
-- `email` is the deliverable address of a user. `reconcile-run-plane` copies
-- it into the `platform."user"` user row of every tenant the user can write
-- in (wamn-0h0g.9.18). Only a user carries one: `principals_email_check`
-- refuses an email on another type and refuses a user without one. `subject`
-- is email-shaped by its own CHECK, but it is an authentication token, so
-- nothing reads it as an address. A service and a platform row take
-- `<subject>@<platform-domain>` in the tenant, built from
-- `registry.meta.platform_domain`, so neither needs a column here.
--
-- `UNIQUE (email)` says the rule plainly: an address names one user. It needs
-- no `type`, because `principals_email_check` already forces NULL on every
-- other type, and PostgreSQL treats repeated NULLs as distinct. So the
-- constraint touches no service or platform row.
--
-- The refusal belongs here, where the address is entered.
-- `platform."user"` is `UNIQUE (tenant_id, email)`, so without this the two
-- users collide inside a tenant instead. That failure stops a whole
-- `reconcile-run-plane` run, unrelated schema repair included.
-- ---------------------------------------------------------------------------
CREATE TABLE identity.principals (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    type         text NOT NULL,
    subject      text NOT NULL,
    email        text,
    display_name text NOT NULL,
    status       text NOT NULL DEFAULT 'active',
    disabled_at  timestamptz,
    created_at   timestamptz NOT NULL,
    created_by   uuid NOT NULL,
    updated_at   timestamptz NOT NULL,
    updated_by   uuid NOT NULL,
    UNIQUE (id, type),
    UNIQUE (type, subject),
    UNIQUE (email),
    CONSTRAINT principals_type_check
        CHECK (type IN ('user', 'service', 'platform')),
    CONSTRAINT principals_subject_check
        CHECK (type = 'platform'
               OR (subject ~ '^[a-z0-9][a-z0-9._@+-]*$'
                   AND char_length(subject) <= 254)),
    CONSTRAINT principals_email_check
        CHECK ((type = 'user') = (email IS NOT NULL)
               AND (email IS NULL
                    OR (email ~ '^[^@[:space:]]+@[^@[:space:]]+\.[^@[:space:]]+$'
                        AND char_length(email) <= 254))),
    CONSTRAINT principals_display_name_check
        CHECK (btrim(display_name) <> '' AND char_length(display_name) <= 200),
    CONSTRAINT principals_status_check
        CHECK (status IN ('active', 'disabled')),
    CONSTRAINT principals_disabled_at_check
        CHECK ((status = 'disabled') = (disabled_at IS NOT NULL)),
    CONSTRAINT principals_platform_principal_check
        CHECK (CASE WHEN type = 'platform'
                    THEN (subject, display_name, id) IN (
                             ('wamn:provisioning', 'wamn:provisioning', '770df186-ac15-579e-b46b-c297cae2011b'::uuid))
                    ELSE display_name NOT LIKE 'wamn:%'
               END)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.principals
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- The wamn:provisioning row is the first identity row. The block binds
-- that principal for the current transaction, so the row stamps itself.
DO $provisioning$ BEGIN
  PERFORM pg_catalog.set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', true);
  INSERT INTO identity.principals (id, type, subject, display_name)
  VALUES ('770df186-ac15-579e-b46b-c297cae2011b', 'platform',
          'wamn:provisioning', 'wamn:provisioning');
END $provisioning$;

CREATE TABLE identity.project_roles (
    principal_id uuid NOT NULL
        REFERENCES identity.principals (id) ON DELETE CASCADE,
    org          text NOT NULL,
    project      text NOT NULL,
    role         text NOT NULL,
    created_at   timestamptz NOT NULL,
    created_by   uuid NOT NULL,
    updated_at   timestamptz NOT NULL,
    updated_by   uuid NOT NULL,
    PRIMARY KEY (principal_id, org, project, role),
    FOREIGN KEY (org, project)
        REFERENCES registry.projects (org, id) ON DELETE CASCADE,
    CONSTRAINT project_roles_role_check
        CHECK (role ~ '^[a-z0-9][a-z0-9-]*$' AND char_length(role) <= 64)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.project_roles
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- ---------------------------------------------------------------------------

-- A user's membership of one org (docs/plan/platform-ui.md §2.7). The
-- principal stays global, and the membership status is org-local: an
-- inactive member keeps the row and holds no access in the org.
CREATE TABLE identity.org_memberships (
    principal_id   uuid NOT NULL,
    principal_type text NOT NULL DEFAULT 'user',
    org            text NOT NULL
        REFERENCES registry.orgs (id) ON DELETE CASCADE,
    status         text NOT NULL,
    created_at     timestamptz NOT NULL,
    created_by     uuid NOT NULL,
    updated_at     timestamptz NOT NULL,
    updated_by     uuid NOT NULL,
    PRIMARY KEY (principal_id, org),
    FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principals (id, type) ON DELETE CASCADE,
    CONSTRAINT org_memberships_user_check
        CHECK (principal_type = 'user'),
    CONSTRAINT org_memberships_status_check
        CHECK (status IN ('active', 'inactive'))
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.org_memberships
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- A user's administrative role in one org (docs/plan/platform-ui.md §4.4).
-- A role needs the user's membership of the org, and deleting the
-- membership deletes the role.
CREATE TABLE identity.org_roles (
    principal_id uuid NOT NULL,
    org          text NOT NULL,
    role         text NOT NULL,
    created_at   timestamptz NOT NULL,
    created_by   uuid NOT NULL,
    updated_at   timestamptz NOT NULL,
    updated_by   uuid NOT NULL,
    PRIMARY KEY (principal_id, org, role),
    FOREIGN KEY (principal_id, org)
        REFERENCES identity.org_memberships (principal_id, org) ON DELETE CASCADE,
    CONSTRAINT org_roles_role_check
        CHECK (role = 'org-admin')
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.org_roles
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- ---------------------------------------------------------------------------
-- Personal access tokens (wamn-ctc8.7) — the opaque bearer presenter both
-- users and services use headlessly. INVARIANT: no token material is stored.
-- `token_prefix` is the non-secret lookup half (hex of the token's random
-- lookup bytes, UNIQUE so verification is a single index probe);
-- `token_hash` is the hex SHA-256 of the WHOLE token string, which is
-- high-entropy random material, so a plain digest is the correct rest form.
-- Expiry is mandatory (no immortal tokens) and revocation is a one-way stamp.
-- The principal FK is deliberately RESTRICT, not CASCADE: a token row is audit
-- evidence that must outlive careless principal deletes. The FK carries the
-- principal type, and `pats_principal_type_check` refuses a platform principal.
-- ---------------------------------------------------------------------------
CREATE TABLE identity.pats (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    principal_id   uuid NOT NULL,
    principal_type text NOT NULL,
    token_prefix   text NOT NULL UNIQUE,
    token_hash     text NOT NULL,
    label          text NOT NULL,
    created_at     timestamptz NOT NULL,
    created_by     uuid NOT NULL,
    updated_at     timestamptz NOT NULL,
    updated_by     uuid NOT NULL,
    expires_at     timestamptz NOT NULL,
    revoked_at     timestamptz,
    FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principals (id, type) ON DELETE RESTRICT,
    CONSTRAINT pats_principal_type_check
        CHECK (principal_type IN ('user', 'service')),
    CONSTRAINT pats_token_prefix_check
        CHECK (token_prefix ~ '^[0-9a-f]{16}$'),
    CONSTRAINT pats_token_hash_check
        CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT pats_label_check
        CHECK (btrim(label) <> '' AND char_length(label) <= 200),
    CONSTRAINT pats_expiry_check
        CHECK (expires_at > created_at)
);

CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.pats
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

CREATE INDEX pats_principal_idx ON identity.pats (principal_id);

-- Password enrollment (wamn-a045.1). Only user principals can enroll.
-- These tables hold credential material and stamps, never row-image history.
-- The identity library serializes enrollment on the principal row and consumes
-- all outstanding invitations in the same transaction as password creation.
-- Scoped issuer grants belong to the password endpoint implementation.
CREATE TABLE identity.password_credentials (
    principal_id uuid PRIMARY KEY,
    principal_type text NOT NULL DEFAULT 'user' CHECK (principal_type = 'user'),
    password_hash text NOT NULL CHECK (octet_length(password_hash) <= 256
                                      AND password_hash LIKE '$argon2id$v=19$%'),
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principals (id, type) ON DELETE RESTRICT
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.password_credentials
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

CREATE TABLE identity.password_tokens (
    token_hash bytea PRIMARY KEY CHECK (octet_length(token_hash) = 32),
    principal_id uuid NOT NULL,
    principal_type text NOT NULL DEFAULT 'user' CHECK (principal_type = 'user'),
    purpose text NOT NULL CHECK (purpose IN ('invitation', 'reset')),
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principals (id, type) ON DELETE RESTRICT,
    CHECK (expires_at > created_at)
);
CREATE INDEX password_tokens_principal_idx ON identity.password_tokens (principal_id);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.password_tokens
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- One password login owns a family of single-use renewal credentials.
-- Credential material gets stamps but never row-image history.
CREATE TABLE identity.password_logins (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    principal_id uuid NOT NULL,
    principal_type text NOT NULL DEFAULT 'user' CHECK (principal_type = 'user'),
    issuer text NOT NULL CHECK (octet_length(issuer) BETWEEN 1 AND 2048),
    audience text NOT NULL CHECK (octet_length(audience) BETWEEN 1 AND 1024),
    authenticated_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    renewal_expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principals (id, type) ON DELETE RESTRICT,
    CHECK (expires_at = authenticated_at + interval '8 hours'),
    CHECK (renewal_expires_at > authenticated_at AND renewal_expires_at <= expires_at)
);
CREATE INDEX password_logins_principal_idx ON identity.password_logins (principal_id);
CREATE INDEX password_logins_expiry_idx ON identity.password_logins (issuer, expires_at);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.password_logins
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

CREATE TABLE identity.renewal_credentials (
    token_hash bytea PRIMARY KEY CHECK (octet_length(token_hash) = 32),
    login_id uuid NOT NULL REFERENCES identity.password_logins (id) ON DELETE CASCADE,
    consumed_at timestamptz,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL
);
CREATE INDEX renewal_credentials_login_idx ON identity.renewal_credentials (login_id);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.renewal_credentials
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- The principal lock orders disablement against every login writer. The trigger
-- also covers administrative status changes outside the identity Rust library.
CREATE FUNCTION identity.revoke_disabled_password_logins() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
    UPDATE identity.password_logins SET revoked_at = clock_timestamp()
    WHERE principal_id = NEW.id AND revoked_at IS NULL;
    RETURN NEW;
END;
$$;
REVOKE ALL ON FUNCTION identity.revoke_disabled_password_logins() FROM PUBLIC;
CREATE TRIGGER revoke_disabled_password_logins
    AFTER UPDATE OF status ON identity.principals
    FOR EACH ROW WHEN (NEW.status = 'disabled')
    EXECUTE FUNCTION identity.revoke_disabled_password_logins();

-- Session signing authority (wamn-ctc8.15.1). This is key-generation state,
-- never per-session state. A fresh UUID kid is generated for each publication;
-- rotation never reuses the credential machinery's A/B slot names as kids.
-- Only the identity authority receives table access. Hosts receive a public
-- projection through JWKS, never a database grant or private key material.
--
-- Signers hold a SHARE lock on their issuer's signing-state row through the
-- signature. Activation and compromise removal hold UPDATE locks on that row.
-- After old signers drain, activation records clock_timestamp() as a confirmed
-- signing cutoff and erases the old private key in the same transaction.
-- The public key remains valid through cutoff + 930 seconds (900 + 30).
-- A failed flip rolls back both state and cutoff; no last-use/token writes are
-- needed. Compromise removal deletes the key immediately and never falls back.
CREATE TABLE identity.session_keys (
    issuer         text NOT NULL CHECK (btrim(issuer) <> ''),
    kid            uuid NOT NULL DEFAULT gen_random_uuid(),
    public_key     bytea NOT NULL CHECK (octet_length(public_key) = 32),
    private_pkcs8  bytea,
    published_at   timestamptz NOT NULL DEFAULT clock_timestamp(),
    signing_cutoff timestamptz,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (issuer, kid),
    CONSTRAINT session_keys_retirement_check
        CHECK ((signing_cutoff IS NULL) = (private_pkcs8 IS NOT NULL))
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.session_keys
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

CREATE TABLE identity.session_signing_state (
    issuer     text PRIMARY KEY CHECK (btrim(issuer) <> ''),
    active_kid uuid,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    FOREIGN KEY (issuer, active_kid)
        REFERENCES identity.session_keys (issuer, kid)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.session_signing_state
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- ---------------------------------------------------------------------------
-- Project-envs — the registry LEAF: one provisioned (org, project, env)
-- database, keyed by the identity triple, with a REFERENCE to its credential
-- Secret. `env` is a validated slug. `policy_name` is the policy its document
-- names, and its composite FK to `registry.env_policies (org, name)` enforces
-- that it names a policy in ITS ORG's set; provisioning records the env
-- name (D18 + 8df.4 — referential integrity replaces the retired
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
--
-- `instance_suffix` is the provision-minted INSTANCE identity (wamn-0h0g.13.57):
-- 8 characters of `[a-z0-9]` randomness, drawn once by `provision-project-env`
-- and never refreshed. The triple names the environment; the suffix distinguishes
-- THIS provisioning of it from an earlier one. Physical names derive from
-- `<triple>--<instance_suffix>` (the Kubernetes namespace today), so deleting an
-- environment and recreating it under the same triple yields different physical
-- names and the new environment cannot inherit whatever survived the delete.
-- Randomness is the whole mechanism — no naming registry, no retry loop, no
-- derivation. It is deliberately NOT part of the PRIMARY KEY: the triple stays
-- the identity every FK here points at (`registry.event_readers`, and the
-- separately installed ops relations), and a re-provisioned environment must
-- REPLACE its row rather than accumulate one per instance.
--
-- `disposable` marks an environment whose admitted facts a development loop may
-- REPLACE (wamn-10yt.38). `wamn dev up` provisions its own target with it true;
-- every other provisioning leaves the `false` default, which is what keeps every
-- existing environment's admitted component fact immutable BY CONSTRUCTION
-- rather than by a caller remembering to withhold a flag. The condition is
-- anchored in THIS row, never in a CLI argument or a stage: the target decides.
-- This row is the AUTHORITY; `catalog.tenant_environments` in the control store
-- carries the projected copy the admit path reads (deploy/sql/control-portable-store.sql).
-- It is deliberately NOT `registry.env_policies.durability_class`, which is
-- run-retention policy for admitted runs and means something else.
--
-- `status` is `active` or `inactive` (docs/plan/platform-ui.md §5.4). Identity
-- offers no audience of an inactive environment, and its host serves none of
-- its routes. CDC, data and grants stay as they are.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.project_envs (
    org              text NOT NULL,
    project          text NOT NULL,
    env              text NOT NULL,
    secret_name      text NOT NULL,
    secret_namespace text,
    instance_suffix  text NOT NULL,
    disposable       boolean NOT NULL DEFAULT false,
    status           text NOT NULL DEFAULT 'active',
    -- The policy name the environment document declares (docs/plan/platform-deploy.md R2, R21).
    policy_name      text NOT NULL,
    -- The route host the environment document declares (docs/plan/platform-deploy.md §10.1).
    route_host       text
        CONSTRAINT project_envs_route_host_check
        CHECK (route_host IS NULL OR route_host ~ '^[a-z0-9]([a-z0-9.-]*[a-z0-9])?$'),
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (org, project, env),
    FOREIGN KEY (org, project) REFERENCES registry.projects (org, id) ON DELETE CASCADE,
    CONSTRAINT project_envs_policy_name_fkey
        FOREIGN KEY (org, policy_name) REFERENCES registry.env_policies (org, name)
        DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT project_envs_instance_suffix_check
        CHECK (instance_suffix ~ '^[a-z0-9]{8}$'),
    CONSTRAINT project_envs_status_check CHECK (status IN ('active', 'inactive'))
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON registry.project_envs
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- User project access is an explicit grant to one project-environment
-- (wamn-ctc8.19). Keep the platform-issued principal UUID: platform."user"
-- uses that same UUID for the environment's roles and permissions. Management
-- project roles do not grant membership, and services use their existing
-- environment-bound authentication. Deleting an environment removes its
-- grants, so provisioning the same triple again cannot inherit them.
CREATE TABLE identity.project_env_memberships (
    principal_id   uuid NOT NULL,
    principal_type text NOT NULL DEFAULT 'user',
    org            text NOT NULL,
    project        text NOT NULL,
    env            text NOT NULL,
    created_at     timestamptz NOT NULL,
    created_by     uuid NOT NULL,
    updated_at     timestamptz NOT NULL,
    updated_by     uuid NOT NULL,
    PRIMARY KEY (principal_id, org, project, env),
    FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principals (id, type) ON DELETE CASCADE,
    FOREIGN KEY (org, project, env)
        REFERENCES registry.project_envs (org, project, env) ON DELETE CASCADE,
    CONSTRAINT project_env_memberships_user_check
        CHECK (principal_type = 'user')
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.project_env_memberships
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- The env_policies → orgs CASCADE is added HERE, after projects/project_envs
-- exist: Postgres fires an org DELETE's cascade triggers in creation order, so
-- adding this FK LAST means the projects → project_envs cascade clears the
-- referencing rows BEFORE the policy rows are deleted — a plain
-- `DELETE FROM registry.orgs WHERE id = ...` tears a whole org down in one
-- statement without tripping the in-use-policy FK above. If a future schema
-- restore reorders the triggers, the DEFERRABLE escape hatch above still works.
ALTER TABLE registry.env_policies
    ADD CONSTRAINT env_policies_org_fkey
    FOREIGN KEY (org) REFERENCES registry.orgs (id) ON DELETE CASCADE;

-- ---------------------------------------------------------------------------
-- Retired project-envs — the handle a SUPERSEDED instance would otherwise not
-- have (wamn-0h0g.15.90). `registry.project_envs` holds exactly one row per
-- triple and a re-provision must REPLACE it, so the instant an environment is
-- deleted the only record of its `instance_suffix` is gone — and with it every
-- name derived from it (the Kubernetes namespace above all). The orphaned
-- namespace and whatever survived inside it then have NO handle: nothing can
-- enumerate them, so nothing can reclaim them.
--
-- This is RECLAIM, not collision-prevention. Randomness remains the entire
-- uniqueness mechanism (wamn-0h0g.13.57) and nothing here is ever consulted when
-- a suffix is minted; this table only remembers what was.
--
-- Deliberately NOT FK'd to registry.project_envs: the whole point is to OUTLIVE the row, and the org/project CASCADE
-- that removes it, so an FK would erase the record at exactly the moment it
-- becomes the only copy. Keyed by (triple, instance_suffix) so a triple
-- accumulates one row per superseded instance and a repeated delete is idempotent.
--
-- Written by a BEFORE DELETE trigger, not by application code: every removal path
-- in the platform is a CASCADE from an org or project DELETE, which runs no
-- application code at all. A trigger is the only place that sees these rows.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.retired_project_envs (
    org             text NOT NULL,
    project         text NOT NULL,
    env             text NOT NULL,
    instance_suffix text NOT NULL,
    retired_at      timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (org, project, env, instance_suffix)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON registry.retired_project_envs
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

CREATE FUNCTION registry.retire_project_env() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO registry.retired_project_envs (org, project, env, instance_suffix)
      VALUES (OLD.org, OLD.project, OLD.env, OLD.instance_suffix)
      ON CONFLICT (org, project, env, instance_suffix) DO NOTHING;
    RETURN OLD;
END $$;

CREATE TRIGGER project_envs_retire_instance
    BEFORE DELETE ON registry.project_envs
    FOR EACH ROW EXECUTE FUNCTION registry.retire_project_env();

-- ---------------------------------------------------------------------------
-- Event readers — the CDC capture registrations (D19 v3, wamn-l5i9.9). One row
-- per project-env with CDC enabled: the publication + failover replication slot
-- the reader streams from (the `wamn_cdc_…` Postgres objects the
-- enable-cdc-project-env overlay provisions), the JetStream stream envelopes
-- land in (one stream per organization, project, and environment), and a
-- REFERENCE to the reader's
-- replication-credential Secret (`wamn-cdc-<org>--<project>--<env>`).
--
-- INVARIANT 2 (no credentials, R8b): `replication_secret_name` (+ optional
-- namespace) is a reference only — the replication credential is its own tier
-- ABOVE the wamn_app query credential, and its material lives in a K8s Secret,
-- never here. Keyed by the identity triple; FK to the project-env it captures
-- through the same project-env identity, so a de-provisioned env (or a deleted
-- org, cascading through project_envs) drops its registration. The reader
-- service (l5i9.10) reads its row to learn what to stream.
--
-- `schema` is the application schema that the publication covers. The reader
-- refuses a registration without one, and a publication of another schema
-- (wamn-0h0g.19.21). It is nullable because a system database from before
-- migration 0002 holds registrations without it, and enable-cdc-project-env
-- writes it on its next run.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.event_readers (
    org                          text NOT NULL,
    project                      text NOT NULL,
    env                          text NOT NULL,
    publication                  text NOT NULL,
    schema                       text[],
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
-- Capture gaps (wamn-59z6, docs/plan/cdc-reader-slot.md section 4.3): one row
-- per lost slot of a CDC reader registration. `wamn-ctl recover-capture-gap`
-- writes it, and `wamn-ctl close-capture-gap` sets `resync_at`. The reader
-- captures only while the newest row of its registration has `resync_at` set,
-- or no row exists. `slot` keeps the lost slot's name. A null `start_lsn`
-- means "from registration": the source stream held no event. `created_at` is
-- the time of the recovery verb.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.capture_gap (
    org        text NOT NULL,
    project    text NOT NULL,
    env        text NOT NULL,
    slot       text NOT NULL,
    start_lsn  pg_lsn,
    start_at   timestamptz NOT NULL,
    reason     text NOT NULL,
    end_lsn    pg_lsn NOT NULL,
    resync_at  timestamptz,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (org, project, env, created_at),
    FOREIGN KEY (org, project, env)
        REFERENCES registry.event_readers (org, project, env) ON DELETE CASCADE
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON registry.capture_gap
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- ---------------------------------------------------------------------------
-- The password fingerprints of the credential generations (wamn-zua8.3,
-- docs/plan/platform-ui.md section 4.10). PostgreSQL shows a password only to a
-- superuser, and the provisioner is not one. A prepare records the SHA-256 of
-- the password that it sets, and a retirement removes the row. A generation is
-- active only with a row. The password itself is stored nowhere.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.generation_passwords (
    role        text PRIMARY KEY,
    sha256      text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    recorded_at timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON registry.generation_passwords
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- The issuer can lock a principal without gaining principal mutation rights.
CREATE FUNCTION identity.lock_password_principal(principal uuid) RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
DECLARE eligible boolean;
BEGIN
    SELECT status = 'active' AND type = 'user' INTO eligible
    FROM identity.principals WHERE id = principal FOR UPDATE;
    RETURN COALESCE(eligible, false);
END;
$$;
REVOKE ALL ON FUNCTION identity.lock_password_principal(uuid) FROM PUBLIC;

-- Fixed-window password admission shared by every identity replica.
CREATE TABLE identity.password_attempts (
    bucket text PRIMARY KEY CHECK (octet_length(bucket) <= 160),
    started_at timestamptz NOT NULL,
    attempts integer NOT NULL CHECK (attempts > 0),
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL
);

-- An environment keeps its target identity.
ALTER TABLE registry.project_envs ADD COLUMN target_id uuid REFERENCES registry.target(id);
CREATE FUNCTION registry.keep_environment_target() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.target_id IS NOT NULL AND NEW.target_id IS DISTINCT FROM OLD.target_id THEN
        RAISE EXCEPTION 'environment_target_changed' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION registry.keep_environment_target() FROM PUBLIC;
CREATE TRIGGER environment_target_immutable BEFORE UPDATE OF target_id ON registry.project_envs
    FOR EACH ROW EXECUTE FUNCTION registry.keep_environment_target();

CREATE TABLE registry.external_system (
    id      uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    org     text NOT NULL,
    project text NOT NULL,
    name    text NOT NULL CHECK (name ~ '^[a-z][a-z0-9_-]{0,62}$'),
    UNIQUE (org, project, name),
    UNIQUE (id, org, project),
    FOREIGN KEY (org, project) REFERENCES registry.projects (org, id)
);
CREATE TABLE registry.integration (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    org                text NOT NULL,
    project            text NOT NULL,
    integration_type   text NOT NULL,
    auth_type          text NOT NULL,
    external_system_id uuid,
    name               text NOT NULL CHECK (name ~ '^[a-z][a-z0-9_-]{0,62}$'),
    status             text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    UNIQUE (org, project, name),
    UNIQUE (id, org, project, integration_type),
    FOREIGN KEY (org, project) REFERENCES registry.projects (org, id),
    FOREIGN KEY (external_system_id, org, project)
        REFERENCES registry.external_system (id, org, project),
    FOREIGN KEY (integration_type, auth_type)
        REFERENCES registry.integration_type_auth (integration_type, auth_type)
);
ALTER TABLE registry.project_envs ADD UNIQUE (org, project, env, target_id);
CREATE TABLE registry.connection (
    id                   uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    integration_id       uuid NOT NULL,
    org                  text NOT NULL,
    project              text NOT NULL,
    env                  text NOT NULL,
    integration_type     text NOT NULL,
    target_id            uuid NOT NULL,
    resource_key         text,
    active_generation    bigint NOT NULL,
    service_principal_id uuid NOT NULL UNIQUE,
    principal_type       text NOT NULL DEFAULT 'service' CHECK (principal_type = 'service'),
    status               text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    UNIQUE (integration_id, env),
    FOREIGN KEY (integration_id, org, project, integration_type)
        REFERENCES registry.integration (id, org, project, integration_type),
    FOREIGN KEY (org, project, env, target_id)
        REFERENCES registry.project_envs (org, project, env, target_id),
    FOREIGN KEY (service_principal_id, principal_type)
        REFERENCES identity.principals (id, type)
);
CREATE UNIQUE INDEX connection_resource_per_target
    ON registry.connection (target_id, resource_key)
    WHERE resource_key IS NOT NULL AND status = 'active';
CREATE TABLE identity.credential (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    connection_id uuid NOT NULL REFERENCES registry.connection (id) ON DELETE CASCADE,
    store         text NOT NULL CHECK (store IN ('k8s', 'file')),
    ref           text NOT NULL,
    version       text NOT NULL,
    hint          text,
    created_at    timestamptz NOT NULL,
    created_by    uuid NOT NULL,
    UNIQUE (id, connection_id)
);
CREATE TABLE registry.connection_generation (
    connection_id uuid NOT NULL REFERENCES registry.connection (id) ON DELETE CASCADE,
    generation    bigint NOT NULL CHECK (generation > 0),
    parameters    jsonb NOT NULL DEFAULT '{}',
    credential_id uuid,
    created_at    timestamptz NOT NULL,
    created_by    uuid NOT NULL,
    PRIMARY KEY (connection_id, generation),
    FOREIGN KEY (credential_id, connection_id)
        REFERENCES identity.credential (id, connection_id)
);
ALTER TABLE registry.connection ADD FOREIGN KEY (id, active_generation)
    REFERENCES registry.connection_generation (connection_id, generation) DEFERRABLE INITIALLY DEFERRED;

CREATE FUNCTION registry.reject_connection_generation_change() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    IF TG_OP = 'DELETE' AND NOT EXISTS (
        SELECT FROM registry.connection WHERE id = OLD.connection_id
    ) THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'connection_generation_is_immutable' USING ERRCODE = '55000';
END $$;
REVOKE ALL ON FUNCTION registry.reject_connection_generation_change() FROM PUBLIC;
CREATE TRIGGER connection_generation_immutable BEFORE UPDATE OR DELETE ON registry.connection_generation
    FOR EACH ROW EXECUTE FUNCTION registry.reject_connection_generation_change();
CREATE TRIGGER credential_immutable BEFORE UPDATE OR DELETE ON identity.credential
    FOR EACH ROW EXECUTE FUNCTION registry.reject_connection_generation_change();

CREATE FUNCTION registry.keep_active_generation() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    IF NEW.active_generation < OLD.active_generation THEN
        RAISE EXCEPTION 'active_generation_cannot_decrease' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION registry.keep_active_generation() FROM PUBLIC;
CREATE TRIGGER connection_generation_forward BEFORE UPDATE OF active_generation ON registry.connection
    FOR EACH ROW EXECUTE FUNCTION registry.keep_active_generation();

CREATE FUNCTION registry.keep_integration_name() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    IF NEW.name IS DISTINCT FROM OLD.name THEN
        RAISE EXCEPTION 'name_is_immutable' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION registry.keep_integration_name() FROM PUBLIC;
CREATE TRIGGER integration_name_immutable BEFORE UPDATE OF name ON registry.integration
    FOR EACH ROW EXECUTE FUNCTION registry.keep_integration_name();
CREATE TRIGGER external_system_name_immutable BEFORE UPDATE OF name ON registry.external_system
    FOR EACH ROW EXECUTE FUNCTION registry.keep_integration_name();

CREATE FUNCTION registry.configure_connection(org_id text, operation text, payload jsonb) RETURNS jsonb
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
#variable_conflict use_variable
DECLARE
    row_name text := payload->>'name';
    project_id text := payload->>'project';
    system_id uuid;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended('connections:' || org_id, 0));
    IF operation = 'external_system.write' THEN
        INSERT INTO registry.external_system (org, project, name)
        VALUES (org_id, project_id, row_name) ON CONFLICT (org, project, name) DO NOTHING;
    ELSIF operation = 'integration.write' THEN
        IF payload->>'external_system' IS NOT NULL THEN
            SELECT id INTO STRICT system_id FROM registry.external_system
            WHERE org = org_id AND project = project_id AND name = payload->>'external_system';
        END IF;
        INSERT INTO registry.integration (org, project, name, integration_type, auth_type, external_system_id, status)
        VALUES (org_id, project_id, row_name, payload->>'integration_type', payload->>'auth_type', system_id,
                coalesce(payload->>'status', 'active'))
        ON CONFLICT (org, project, name) DO UPDATE
        SET status = EXCLUDED.status;
    ELSE
        RETURN jsonb_build_object('error', 'invalid_input', 'field', 'operation');
    END IF;
    RETURN jsonb_build_object('name', row_name);
END $$;

CREATE FUNCTION registry.publish_integration(
    integration_code text, published_version integer, artifact_digest text,
    parameters jsonb, exported_operations text)
RETURNS void LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog AS $$
    INSERT INTO registry.integration_type_version
        (integration_type, version, digest, parameter_schema, operations, published_at)
    VALUES (integration_code, published_version, artifact_digest, parameters, exported_operations, transaction_timestamp())
$$;

DO $connection_authority_acl$
DECLARE signature text;
BEGIN
    FOREACH signature IN ARRAY ARRAY[
        'configure_connection(text,text,jsonb)',
        'publish_integration(text,integer,text,jsonb,text)'
    ] LOOP
        EXECUTE 'ALTER FUNCTION registry.' || signature || ' OWNER TO wamn_system';
        EXECUTE 'REVOKE ALL ON FUNCTION registry.' || signature || ' FROM PUBLIC';
        IF to_regrole('wamn_control') IS NOT NULL THEN
            GRANT USAGE ON SCHEMA registry TO wamn_control;
            EXECUTE 'GRANT EXECUTE ON FUNCTION registry.' || signature || ' TO wamn_control';
        END IF;
        IF to_regrole('wamn_administration') IS NOT NULL THEN
            GRANT USAGE ON SCHEMA registry TO wamn_administration;
            EXECUTE 'GRANT EXECUTE ON FUNCTION registry.' || signature || ' TO wamn_administration';
        END IF;
    END LOOP;
END $connection_authority_acl$;

CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.password_attempts
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

DO $control_surface$ BEGIN
  IF EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_control') THEN
    REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; REVOKE ALL PRIVILEGES ON SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; GRANT USAGE ON SCHEMA "identity", "registry" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_roles" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."password_logins" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."principals" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_env_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_roles" TO "wamn_control"; GRANT SELECT ON TABLE "registry"."project_envs" TO "wamn_control"; GRANT SELECT, INSERT ON TABLE "registry"."projects" TO "wamn_control"; GRANT UPDATE ("status") ON TABLE "registry"."project_envs" TO "wamn_control"; DO $connection_acl$ BEGIN IF to_regprocedure('registry.publish_integration(text,integer,text,jsonb,text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.publish_integration(text,integer,text,jsonb,text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.configure_connection(text,text,jsonb)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.configure_connection(text,text,jsonb) TO "wamn_control"; END IF; END $connection_acl$;
  END IF;
END $control_surface$;
