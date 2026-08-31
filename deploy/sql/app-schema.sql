-- The per-project SYSTEM SCHEMA v1 (wamn-as5, docs/archive/platform-plan.md §2.4). The
-- application-facing auth/RBAC/config tables that live IN a project database:
-- users, roles (+ the user↔role linkage), permissions, configurations,
-- audit_log, and api_keys.
--
-- This is the AUTH/RBAC half of item 2.4. Package coordinates, immutable
-- migration ledgers, effective releases, and release membership are already
-- shipped by deploy/sql/catalog-schema.sql and are deliberately not redefined
-- here. A
-- `deployments` table is deferred — a live WorkloadDeployment is a K8s CR, so a
-- registry table would duplicate cluster state until there is a concrete reader
-- (follow-up bead).
--
-- DISTINCT FROM the T1 control-plane registry (deploy/sql/system-schema.sql,
-- wamn-q3n.3): that is the PLATFORM-GLOBAL system DB (orgs / projects /
-- project_envs / sagas), owned by wamn_system, NOT tenant-scoped, NO RLS floor.
-- THIS schema is PER-PROJECT TENANT DATA: tenant-scoped, wamn_app-readable, under
-- the same RLS floor as the catalog and the generated data tables. wamn_app's
-- WRITE authority is NOT uniform across these tables — see WRITE AUTHORITY
-- below. Different plane, different owner, different security model — hence a
-- different file, and deliberately NOT named system-schema.sql.
--
-- STANDALONE ARTIFACT: like deploy/sql/catalog-schema.sql, this file is NOT included
-- by deploy/sql/postgres-init.sql (which builds the S2–S6 gate fixtures). It is the
-- persistence target a project database provisions; shipping it here keeps the
-- 2.4 schema reviewable in one place without touching the gate fixtures. It
-- assumes a pre-existing wamn_app role (NOSUPERUSER, no BYPASSRLS), as in
-- production and as the live-apply gate provisions.
--
-- SECURITY SHAPE mirrors deploy/sql/catalog-schema.sql exactly (the 3.2 tenant
-- floor): one stable ACL role (wamn_app, not owner), tenant separation from
-- `current_user` (wamn-0h0g.22.6). Every table FORCEs RLS keyed on
--   wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()
-- and carries the matching `<table>_tkey` expression index. A role outside the
-- guest generation convention derives NULL and matches no row, and
-- CHECK (tenant_id <> '') forbids a ''-tenant row, so the floor is structural.
--
-- WRITE AUTHORITY (R11) splits these seven tables into three classes. RLS answers
-- "which rows"; the GRANT answers "which relations at all", and the two questions
-- have different answers here:
--
--   PLATFORM-PROTECTED — users, roles, user_roles, permissions, api_keys.
--   SELECT only. The trust chain consumes these rows as authorization INPUT: the
--   3.5 RLS builder compiles app.user_id and app.role into the data policies it
--   generates, and 4.2 resolves both claims from users + user_roles. Author SQL
--   holding DML here could write itself the credential its own policies are then
--   evaluated against, and the compiler would faithfully enforce the forgery. A
--   tenant's authorization state is written BY the platform, never by the
--   tenant's own SQL. There is no wamn_app writer to lose today: the write path
--   is platform-pending — 4.2 (wamn-0xd) resolves the claims from these rows and
--   the first-party identity authority (wamn-ctc8) mints them, both on a
--   platform identity, not on this grant.
--
--   TENANT BUSINESS STATE — configurations. Full DML, deliberately retained.
--   Nothing in the trust chain reads it; it is the project's own settings
--   surface, and narrowing it would protect the platform from data the platform
--   never consumes.
--
--   APPEND-ONLY AGAINST THE AUDITED PARTY — audit_log. SELECT + INSERT, no
--   UPDATE/DELETE. Appending to your own history is the point; rewriting or
--   erasing it is exactly what the word "audit" forbids.
--
-- TRUNCATE is not mentioned anywhere below because it was never granted to
-- wamn_app: only the table owner holds it.
--
-- CLAIM INTEGRATION (3.5 / 4.2): `users.id` is a `uuid` — the ownership target
-- the 3.5 RLS builder reads as NULLIF(current_setting('app.user_id', true),
-- '')::uuid, so a data table's owner column referencing a user resolves. Role
-- NAMES (`roles.name`, text) are what the builder reads as
-- COALESCE(current_setting('app.role', true), '') IN (...). The claims
-- themselves are INJECTED by the plugin from a resolved user/session (4.2) —
-- this schema is the SUBSTRATE, not the auth logic: NO password hashing / JWT /
-- session management here. `api_keys.key_hash` is a one-way digest column (the
-- raw key is shown once at creation, never stored; hashing is 4.2's job).

CREATE SCHEMA app_system AUTHORIZATION postgres;

-- The shared platform group role every non-guest tenant-floor arm below targets
-- (`wamn-0h0g.22.17`). Created HERE, not assumed, for the reason
-- `crates/control/provision/src/tenant_key.rs` already gives for its own
-- bootstrap: naming a role that may not exist adds a new precondition to every
-- applier — seven live gates plus the production path in `services/ctl` — and
-- `CREATE POLICY ... TO wamn_platform` fails outright against a cluster without
-- it.
--
-- EXCEPTION-guarded under the shared `wamn_role_bootstrap` advisory lock, the
-- `ensure_platform_group_role_sql` shape. Roles are CLUSTER-global, so two
-- appliers that do not both take the lock can each observe the role absent and
-- both issue `CREATE ROLE`.
DO $platform_group$ BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles
                 WHERE rolname = 'wamn_platform') THEN
    CREATE ROLE wamn_platform NOLOGIN NOSUPERUSER NOCREATEDB
      NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
  END IF;
EXCEPTION WHEN duplicate_object THEN NULL;
END $platform_group$;
GRANT USAGE ON SCHEMA app_system TO wamn_app;

-- ---------------------------------------------------------------------------
-- The per-database authority derivations every tenant policy below calls
-- (`wamn-0h0g.22.6`). Guest tenant authority comes from `current_user`, not
-- from a claim the session can set: a session that can set its own tenant can
-- read every tenant.
--
-- GENERATED — this block is `authority_derivations_bootstrap_sql()` verbatim,
-- pinned by a byte-equality guard in wamn-control-provision. Do not hand-edit;
-- change the builder.
--
-- It is the BOOTSTRAP rendering because this file is applied to databases whose
-- names are not known here, and `tenant_key` must bake the name in as a literal
-- to stay `IMMUTABLE` — without which the expression indexes below are not even
-- creatable.
-- ---------------------------------------------------------------------------
DO $wamn_authority_bootstrap$
BEGIN
    EXECUTE replace(replace($wamn_authority_derivations$CREATE SCHEMA IF NOT EXISTS "wamn_authority" AUTHORIZATION CURRENT_USER;
REVOKE ALL ON SCHEMA "wamn_authority" FROM PUBLIC;
GRANT USAGE ON SCHEMA "wamn_authority" TO "wamn_app";
CREATE OR REPLACE FUNCTION "wamn_authority".tenant_key(tenant text)
RETURNS text
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
STRICT
SET search_path = pg_catalog
AS $$
    SELECT substr(encode(sha256(
           int8send(19::bigint) || convert_to('wamn.app.scope.v0.1', 'UTF8')
        || int8send(6::bigint) || convert_to('tenant', 'UTF8')
        || int8send(octet_length(convert_to(tenant, 'UTF8'))::bigint) || convert_to(tenant, 'UTF8')
        || int8send(8::bigint) || convert_to('database', 'UTF8')
        || int8send(@wamn_database_octets@::bigint) || convert_to(@wamn_database_literal@, 'UTF8')
       ), 'hex'), 1, 40)
$$;
ALTER FUNCTION "wamn_authority".tenant_key(text) OWNER TO CURRENT_USER;
REVOKE ALL ON FUNCTION "wamn_authority".tenant_key(text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION "wamn_authority".tenant_key(text) TO "wamn_app";
CREATE OR REPLACE FUNCTION "wamn_authority".current_tenant_key()
RETURNS text
LANGUAGE sql
STABLE
PARALLEL SAFE
SET search_path = pg_catalog
AS $$
    SELECT substring(current_user::text from '^wamn_app_([0-9a-f]{40})_[ab]$')
$$;
ALTER FUNCTION "wamn_authority".current_tenant_key() OWNER TO CURRENT_USER;
REVOKE ALL ON FUNCTION "wamn_authority".current_tenant_key() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION "wamn_authority".current_tenant_key() TO "wamn_app";$wamn_authority_derivations$,
        '@wamn_database_literal@', quote_literal(current_database())),
        '@wamn_database_octets@', octet_length(convert_to(current_database(), 'UTF8'))::text);
END
$wamn_authority_bootstrap$;

-- ---------------------------------------------------------------------------
-- Users — application accounts. `id` (uuid) is org-issued at birth and has no
-- database default; it is the app.user_id ownership target (3.5). Identity
-- only: no credential material lives here (auth is 4.2/8.1).
-- `status` gates whether the account may authenticate (enforced by 4.2, not this
-- schema). Email is unique within a tenant.
-- ---------------------------------------------------------------------------
CREATE TABLE app_system.users (
    tenant_id    text NOT NULL CHECK (tenant_id <> ''),
    id           uuid NOT NULL,
    email        text NOT NULL,
    display_name text,
    status       text NOT NULL DEFAULT 'active',
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, id),
    UNIQUE (tenant_id, email),
    CONSTRAINT users_status_check
        CHECK (status IN ('active', 'disabled', 'invited'))
);
ALTER TABLE app_system.users ENABLE ROW LEVEL SECURITY;
ALTER TABLE app_system.users FORCE ROW LEVEL SECURITY;
CREATE POLICY users_tenant ON app_system.users
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY users_platform ON app_system.users
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX users_tkey
    ON app_system.users ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT ON app_system.users TO wamn_app;

-- ---------------------------------------------------------------------------
-- Roles — named roles. `name` is the app.role value the 3.5 RLS builder compares
-- (a role gate is COALESCE(app.role,'') IN ('r1', …)), so the NAME is the
-- load-bearing identity (the composite PK). `is_system` = platform-provided,
-- structure-locked (e.g. a default `admin`); custom roles are tenant-authored.
-- ---------------------------------------------------------------------------
CREATE TABLE app_system.roles (
    tenant_id   text NOT NULL CHECK (tenant_id <> ''),
    name        text NOT NULL,
    description text,
    is_system   boolean NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, name)
);
ALTER TABLE app_system.roles ENABLE ROW LEVEL SECURITY;
ALTER TABLE app_system.roles FORCE ROW LEVEL SECURITY;
CREATE POLICY roles_tenant ON app_system.roles
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY roles_platform ON app_system.roles
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX roles_tkey
    ON app_system.roles ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT ON app_system.roles TO wamn_app;

-- ---------------------------------------------------------------------------
-- User↔role linkage (many-to-many). 4.2 reads this to compute a user's role for
-- the app.role claim it injects. Both sides FK ON DELETE CASCADE within the
-- tenant, so removing a user or a role prunes the grant.
-- ---------------------------------------------------------------------------
CREATE TABLE app_system.user_roles (
    tenant_id  text NOT NULL CHECK (tenant_id <> ''),
    user_id    uuid NOT NULL,
    role_name  text NOT NULL,
    granted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id, role_name),
    FOREIGN KEY (tenant_id, user_id)
        REFERENCES app_system.users (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, role_name)
        REFERENCES app_system.roles (tenant_id, name) ON DELETE CASCADE
);
ALTER TABLE app_system.user_roles ENABLE ROW LEVEL SECURITY;
ALTER TABLE app_system.user_roles FORCE ROW LEVEL SECURITY;
CREATE POLICY user_roles_tenant ON app_system.user_roles
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY user_roles_platform ON app_system.user_roles
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX user_roles_tkey
    ON app_system.user_roles ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT ON app_system.user_roles TO wamn_app;

-- ---------------------------------------------------------------------------
-- Permissions — the grants a role carries (role → permission string, e.g.
-- 'receipts:read'). 4.3 AuthZ reads role → permissions. FK to roles ON DELETE
-- CASCADE within the tenant.
-- ---------------------------------------------------------------------------
CREATE TABLE app_system.permissions (
    tenant_id  text NOT NULL CHECK (tenant_id <> ''),
    role_name  text NOT NULL,
    permission text NOT NULL,
    PRIMARY KEY (tenant_id, role_name, permission),
    FOREIGN KEY (tenant_id, role_name)
        REFERENCES app_system.roles (tenant_id, name) ON DELETE CASCADE
);
ALTER TABLE app_system.permissions ENABLE ROW LEVEL SECURITY;
ALTER TABLE app_system.permissions FORCE ROW LEVEL SECURITY;
CREATE POLICY permissions_tenant ON app_system.permissions
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY permissions_platform ON app_system.permissions
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX permissions_tkey
    ON app_system.permissions ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT ON app_system.permissions TO wamn_app;

-- ---------------------------------------------------------------------------
-- Configurations — per-project application settings, keyed by string. `config_value`
-- is opaque jsonb (the schema does not interpret it). Distinctive column names
-- (config_key / config_value) avoid the reserved-ish `key` / `value`.
-- ---------------------------------------------------------------------------
CREATE TABLE app_system.configurations (
    tenant_id    text NOT NULL CHECK (tenant_id <> ''),
    config_key   text NOT NULL,
    config_value jsonb NOT NULL,
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, config_key)
);
ALTER TABLE app_system.configurations ENABLE ROW LEVEL SECURITY;
ALTER TABLE app_system.configurations FORCE ROW LEVEL SECURITY;
CREATE POLICY configurations_tenant ON app_system.configurations
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY configurations_platform ON app_system.configurations
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX configurations_tkey
    ON app_system.configurations ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT, INSERT, UPDATE, DELETE ON app_system.configurations TO wamn_app;

-- ---------------------------------------------------------------------------
-- Audit log — append-only trail of who did what. Append-only is ENFORCED by the
-- grant below (R11 class 3): wamn_app holds SELECT + INSERT and no UPDATE/DELETE,
-- so the audited party can add to its history but cannot rewrite or erase it. The
-- missing FK is the other half: `actor_id` is a bare uuid, NOT FK'd to users, so
-- the history SURVIVES deletion of the user it references (a cascade would erase
-- the very record of that user's actions). Both halves are against wamn_app — the
-- table owner is unconstrained, as it must be to run migrations and retention.
-- `actor_id` is nullable for system/anonymous actions. `detail` is structured
-- jsonb context. Indexed by (tenant_id, occurred_at) for the time-range scan.
-- ---------------------------------------------------------------------------
CREATE TABLE app_system.audit_log (
    tenant_id   text NOT NULL CHECK (tenant_id <> ''),
    id          uuid NOT NULL DEFAULT gen_random_uuid(),
    actor_id    uuid,
    action      text NOT NULL,
    target      text,
    detail      jsonb,
    occurred_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, id)
);
ALTER TABLE app_system.audit_log ENABLE ROW LEVEL SECURITY;
ALTER TABLE app_system.audit_log FORCE ROW LEVEL SECURITY;
CREATE POLICY audit_log_tenant ON app_system.audit_log
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY audit_log_platform ON app_system.audit_log
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX audit_log_tkey
    ON app_system.audit_log ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT, INSERT ON app_system.audit_log TO wamn_app;
CREATE INDEX audit_log_occurred ON app_system.audit_log (tenant_id, occurred_at);

-- ---------------------------------------------------------------------------
-- API keys — the api-key substrate. `key_hash` is a one-way digest (the raw key
-- is shown once at creation, never stored); `prefix` is a short NON-secret
-- lookup prefix so verification finds the candidate row before the hash compare.
-- A key authenticates AS a user (FK ON DELETE CASCADE). `revoked_at` /
-- `expires_at` gate validity (enforced by 4.2). key_hash is unique per tenant.
-- ---------------------------------------------------------------------------
CREATE TABLE app_system.api_keys (
    tenant_id    text NOT NULL CHECK (tenant_id <> ''),
    id           uuid NOT NULL DEFAULT gen_random_uuid(),
    user_id      uuid NOT NULL,
    name         text NOT NULL,
    key_hash     text NOT NULL,
    prefix       text NOT NULL,
    last_used_at timestamptz,
    expires_at   timestamptz,
    revoked_at   timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, id),
    UNIQUE (tenant_id, key_hash),
    FOREIGN KEY (tenant_id, user_id)
        REFERENCES app_system.users (tenant_id, id) ON DELETE CASCADE
);
ALTER TABLE app_system.api_keys ENABLE ROW LEVEL SECURITY;
ALTER TABLE app_system.api_keys FORCE ROW LEVEL SECURITY;
CREATE POLICY api_keys_tenant ON app_system.api_keys
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY api_keys_platform ON app_system.api_keys
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX api_keys_tkey
    ON app_system.api_keys ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT ON app_system.api_keys TO wamn_app;
