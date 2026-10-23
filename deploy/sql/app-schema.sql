-- The per-project SYSTEM SCHEMA v1 (wamn-as5, docs/archive/platform-plan.md §2.4). The
-- application-facing auth/RBAC/config tables that live IN a project database:
-- user, roles (+ the user↔role linkage), permissions, api_keys, and
-- environment, and the platform configuration tables of
-- docs/plan/platform-config.md (PLATFORM CONFIGURATION, at the end).
--
-- This is the AUTH/RBAC half of item 2.4. Package coordinates, immutable
-- migration tables, effective releases, and release membership are already
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
-- RECORD HISTORY: every table carries created_at, created_by, updated_at, and
-- updated_by as NOT NULL columns, and a wamn_record_history_stamp trigger that calls
-- wamn_history.stamp_row(). An applier installs deploy/sql/record-history.sql
-- first. CATALOG_SCHEMA_SQL carries it, so an applier that applies the catalog
-- schema first already has it. Every write binds app.user_id: provisioning
-- binds wamn:provisioning, apply-package binds wamn:apply-package, and
-- administrative SQL and test fixtures bind a provisioned principal.
-- Every table also keeps an unlimited log. Its wamn_record_history_log trigger
-- writes each row change to <table>_history, so every write also binds
-- app.operation. The HISTORY TABLES section at the end creates them.
--
-- SECURITY SHAPE mirrors deploy/sql/catalog-schema.sql exactly (the 3.2 tenant
-- floor): one stable ACL role (wamn_app, not owner), tenant separation from
-- `current_user` (wamn-0h0g.22.6). Every table FORCEs RLS keyed on
--   wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()
-- and carries the matching `<table>_tkey` expression index. A role outside the
-- guest generation convention derives NULL and matches no row, and
-- CHECK (tenant_id <> '') forbids a ''-tenant row, so the floor is structural.
-- A history table has the same policies and index. It has no such CHECK, and
-- it copies tenant_id from a row of its base table.
--
-- WRITE AUTHORITY (R11) splits these tables into four classes. RLS answers
-- "which rows"; the GRANT answers "which relations at all", and the two questions
-- have different answers here:
--
--   PLATFORM-PROTECTED — user, roles, user_roles, permissions, environment,
--   and the platform configuration tables.
--   SELECT only. The trust chain consumes these rows as authorization INPUT: the
--   3.5 RLS builder compiles app.user_id and app.role into the data policies it
--   generates, and 4.2 resolves both claims from user + user_roles. Author SQL
--   holding DML here could write itself the credential its own policies are then
--   evaluated against, and the compiler would faithfully enforce the forgery. A
--   tenant's authorization state is written BY the platform, never by the
--   tenant's own SQL. There is no wamn_app writer to lose today: the write path
--   is platform-pending — 4.2 (wamn-0xd) resolves the claims from these rows and
--   the first-party identity authority (wamn-ctc8) mints them, both on a
--   platform identity, not on this grant. The application advances
--   number_counter only through platform.bump_counter, the one routine it may
--   EXECUTE in this schema.
--
--   NO GRANT — api_keys. No runtime code reads it, so wamn_app holds nothing
--   on it.
--
--   WRITE LOG — write_log. INSERT, SELECT, UPDATE (result) and DELETE only.
--   The engine claims the key of every create, update, delete and command here. It is outside
--   the tenant floor and the record history: see THE WRITE LOG below.
--
--   HISTORY: the <table>_history tables. No grant. The log trigger fires as
--   the writer, and wamn_app writes no table that has a history table.
--
-- TRUNCATE is not mentioned anywhere below because it was never granted to
-- wamn_app: only the table owner holds it.
--
-- CLAIM INTEGRATION (3.5 / 4.2): `user.id` is a `uuid` — the ownership target
-- the 3.5 RLS builder reads as NULLIF(current_setting('app.user_id', true),
-- '')::uuid, so a data table's owner column referencing a user resolves. Role
-- NAMES (`roles.name`, text) are what the builder reads as
-- COALESCE(current_setting('app.role', true), '') IN (...). The claims
-- themselves are INJECTED by the plugin from a resolved user/session (4.2) —
-- this schema is the SUBSTRATE, not the auth logic: NO password hashing / JWT /
-- session management here. `api_keys.key_hash` is a one-way digest column (the
-- raw key is shown once at creation, never stored; hashing is 4.2's job).

CREATE SCHEMA platform AUTHORIZATION CURRENT_USER;

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
GRANT USAGE ON SCHEMA platform TO wamn_app;

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
-- User — application accounts. `id` (uuid) is org-issued at birth and has no
-- database default; it is the app.user_id ownership target (3.5). Identity
-- only: no credential material lives here (auth is 4.2/8.1).
-- `status` gates whether the account may authenticate (enforced by 4.2, not this
-- schema). Email is unique within a tenant.
--
-- `type` names the kind of principal: a user, a service (a station or an
-- integration), or a platform component. It has no default, so every insert
-- names its type. A platform row carries its `wamn:<component>` name in
-- `display_name` and the id that `wamn-project-state` derives from that name.
-- `user_platform_principal_check` pins each (display_name, id) pair and
-- refuses a `wamn:` name on any other type. PostgreSQL has no UUIDv5, so a
-- Rust test compares these literals with the derivation. Provisioning writes
-- the platform rows and their `<component>@<platform-domain>` email.
-- ---------------------------------------------------------------------------
CREATE TABLE platform."user" (
    tenant_id    text NOT NULL CHECK (tenant_id <> ''),
    id           uuid NOT NULL,
    type         text NOT NULL,
    email        text NOT NULL,
    display_name text,
    status       text NOT NULL DEFAULT 'active',
    created_at   timestamptz NOT NULL,
    created_by   uuid NOT NULL,
    updated_at   timestamptz NOT NULL,
    updated_by   uuid NOT NULL,
    PRIMARY KEY (tenant_id, id),
    UNIQUE (tenant_id, email),
    CONSTRAINT user_status_check
        CHECK (status IN ('active', 'disabled', 'invited')),
    CONSTRAINT user_type_check
        CHECK (type IN ('user', 'service', 'platform')),
    CONSTRAINT user_platform_principal_check
        CHECK (CASE WHEN type = 'platform'
                    THEN COALESCE((display_name, id) IN (
                             ('wamn:provisioning', '770df186-ac15-579e-b46b-c297cae2011b'::uuid),
                             ('wamn:apply-package', '7695180f-4b9a-581f-84ef-d7e9cdbd2b77'::uuid),
                             ('wamn:materializer', '968bd0cc-e612-5d29-9d6c-af1993b8df0a'::uuid),
                             ('wamn:executor', 'd318d033-29ea-5cb0-ab56-24340413fbcc'::uuid),
                             ('wamn:audit-retention', '34cbd151-990e-5800-93be-4fba8878b943'::uuid)),
                         false)
                    ELSE display_name IS NULL OR display_name NOT LIKE 'wamn:%'
               END)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON platform."user"
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');
ALTER TABLE platform."user" ENABLE ROW LEVEL SECURITY;
ALTER TABLE platform."user" FORCE ROW LEVEL SECURITY;
CREATE POLICY user_tenant ON platform."user"
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY user_platform ON platform."user"
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX user_tkey
    ON platform."user" ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT ON platform."user" TO wamn_app;

-- ---------------------------------------------------------------------------
-- Roles — named roles. `name` is the app.role value the 3.5 RLS builder compares
-- (a role gate is COALESCE(app.role,'') IN ('r1', …)), so the NAME is the
-- load-bearing identity (the composite PK). `admin` is the one built-in role:
-- apply-package writes it, and it holds every operation the serving release
-- serves with no permission rows. Every other role is authored. A user or
-- service holds a role through `wamn-ctl grant-role`. `roles_name_check` is the
-- role slug of identity (`wamn_session::token::is_role_slug`), so no stored
-- role name is one that a session refuses.
-- ---------------------------------------------------------------------------
CREATE TABLE platform.roles (
    tenant_id   text NOT NULL CHECK (tenant_id <> ''),
    name        text NOT NULL,
    description text,
    created_at  timestamptz NOT NULL,
    created_by  uuid NOT NULL,
    updated_at  timestamptz NOT NULL,
    updated_by  uuid NOT NULL,
    PRIMARY KEY (tenant_id, name),
    CONSTRAINT roles_name_check
        CHECK (name ~ '^[a-z0-9][a-z0-9-]*$' AND octet_length(name) <= 64)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON platform.roles
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');
ALTER TABLE platform.roles ENABLE ROW LEVEL SECURITY;
ALTER TABLE platform.roles FORCE ROW LEVEL SECURITY;
CREATE POLICY roles_tenant ON platform.roles
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY roles_platform ON platform.roles
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX roles_tkey
    ON platform.roles ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT ON platform.roles TO wamn_app;

-- ---------------------------------------------------------------------------
-- User↔role linkage (many-to-many). 4.2 reads this to compute a user's role for
-- the app.role claim it injects. Both sides FK ON DELETE CASCADE within the
-- tenant, so removing a user or a role prunes the grant.
-- ---------------------------------------------------------------------------
CREATE TABLE platform.user_roles (
    tenant_id  text NOT NULL CHECK (tenant_id <> ''),
    user_id    uuid NOT NULL,
    role_name  text NOT NULL,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (tenant_id, user_id, role_name),
    FOREIGN KEY (tenant_id, user_id)
        REFERENCES platform."user" (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, role_name)
        REFERENCES platform.roles (tenant_id, name) ON DELETE CASCADE
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON platform.user_roles
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');
ALTER TABLE platform.user_roles ENABLE ROW LEVEL SECURITY;
ALTER TABLE platform.user_roles FORCE ROW LEVEL SECURITY;
CREATE POLICY user_roles_tenant ON platform.user_roles
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY user_roles_platform ON platform.user_roles
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX user_roles_tkey
    ON platform.user_roles ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT ON platform.user_roles TO wamn_app;

-- ---------------------------------------------------------------------------
-- Permissions — the operations an authored role holds
-- (docs/plan/trx-pkg-plan.md, Authorization). `permission` is a stable
-- operation reference `<package>:<interface>/<operation>`, with no package
-- version. A caller holds exactly the rows of its roles; nothing derives
-- into them. A row that the serving release does not serve grants nothing.
-- `admin` has no rows. FK to roles ON DELETE CASCADE within the tenant.
-- ---------------------------------------------------------------------------
CREATE TABLE platform.permissions (
    tenant_id   text NOT NULL CHECK (tenant_id <> ''),
    role_name   text NOT NULL,
    permission  text NOT NULL,
    created_at  timestamptz NOT NULL,
    created_by  uuid NOT NULL,
    updated_at  timestamptz NOT NULL,
    updated_by  uuid NOT NULL,
    PRIMARY KEY (tenant_id, role_name, permission),
    FOREIGN KEY (tenant_id, role_name)
        REFERENCES platform.roles (tenant_id, name) ON DELETE CASCADE,
    CONSTRAINT permissions_admin_check CHECK (role_name <> 'admin'),
    CONSTRAINT permissions_reference_check
        CHECK (permission ~ '^[a-z][a-z0-9]*(-[a-z0-9]+)*:[a-z][a-z0-9]*(-[a-z0-9]+)*/[a-z][a-z0-9]*(-[a-z0-9]+)*$')
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON platform.permissions
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');
ALTER TABLE platform.permissions ENABLE ROW LEVEL SECURITY;
ALTER TABLE platform.permissions FORCE ROW LEVEL SECURITY;
CREATE POLICY permissions_tenant ON platform.permissions
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY permissions_platform ON platform.permissions
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX permissions_tkey
    ON platform.permissions ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT ON platform.permissions TO wamn_app;

-- ---------------------------------------------------------------------------
-- API keys — the api-key substrate. `key_hash` is a one-way digest (the raw key
-- is shown once at creation, never stored); `prefix` is a short NON-secret
-- lookup prefix so verification finds the candidate row before the hash compare.
-- A key authenticates AS a user (FK ON DELETE CASCADE). `revoked_at` /
-- `expires_at` gate validity (enforced by 4.2). key_hash is unique per tenant.
-- ---------------------------------------------------------------------------
CREATE TABLE platform.api_keys (
    tenant_id    text NOT NULL CHECK (tenant_id <> ''),
    id           uuid NOT NULL DEFAULT gen_random_uuid(),
    user_id      uuid NOT NULL,
    name         text NOT NULL,
    key_hash     text NOT NULL,
    prefix       text NOT NULL,
    last_used_at timestamptz,
    expires_at   timestamptz,
    revoked_at   timestamptz,
    created_at   timestamptz NOT NULL,
    created_by   uuid NOT NULL,
    updated_at   timestamptz NOT NULL,
    updated_by   uuid NOT NULL,
    PRIMARY KEY (tenant_id, id),
    UNIQUE (tenant_id, key_hash),
    FOREIGN KEY (tenant_id, user_id)
        REFERENCES platform."user" (tenant_id, id) ON DELETE CASCADE
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON platform.api_keys
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');
ALTER TABLE platform.api_keys ENABLE ROW LEVEL SECURITY;
ALTER TABLE platform.api_keys FORCE ROW LEVEL SECURITY;
CREATE POLICY api_keys_tenant ON platform.api_keys
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY api_keys_platform ON platform.api_keys
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX api_keys_tkey
    ON platform.api_keys ((wamn_authority.tenant_key(tenant_id)));

-- ---------------------------------------------------------------------------
-- Environment — the status of this environment, mirrored from
-- registry.project_envs.status in the system database (docs/plan/platform-ui.md
-- §5.4, owner rulings of 2026-10-02 on wamn-zua8.2). The host reads it as
-- wamn_app on each request and answers environment-inactive while it is
-- inactive. No row means active: the first status route of the control host
-- writes the row through the administration login.
-- ---------------------------------------------------------------------------
CREATE TABLE platform.environment (
    tenant_id  text NOT NULL CHECK (tenant_id <> ''),
    status     text NOT NULL CHECK (status IN ('active', 'inactive')),
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (tenant_id)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON platform.environment
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');
ALTER TABLE platform.environment ENABLE ROW LEVEL SECURITY;
ALTER TABLE platform.environment FORCE ROW LEVEL SECURITY;
CREATE POLICY environment_tenant ON platform.environment
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY environment_platform ON platform.environment
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX environment_tkey
    ON platform.environment ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT ON platform.environment TO wamn_app;

-- ---------------------------------------------------------------------------
-- HISTORY TABLES. wamn_history.create_history_table creates <table>_history
-- for each table with the tenant flag, so each history table has tenant_id.
-- The applier owns each history table. Each history table has the tenant
-- floor of its base table: FORCEd RLS, a <table>_history_tenant policy TO
-- wamn_app, a <table>_history_platform arm TO wamn_platform, and a
-- <table>_history_tkey index. A wamn_platform member that writes a table
-- needs the platform arm to append an entry. Each wamn_record_history_log trigger
-- has the argument 'unlimited', the shape that apply-package installs, so the
-- audit retention role gets no grant on these history tables.
-- ---------------------------------------------------------------------------
DO $history_tables$ BEGIN
  PERFORM wamn_history.create_history_table('platform', relation, true)
     FROM unnest(ARRAY['user', 'roles', 'user_roles', 'permissions',
                       'api_keys', 'environment']) AS relation;
END $history_tables$;

ALTER TABLE platform.user_history ENABLE ROW LEVEL SECURITY;
ALTER TABLE platform.user_history FORCE ROW LEVEL SECURITY;
CREATE POLICY user_history_tenant ON platform.user_history
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY user_history_platform ON platform.user_history
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX user_history_tkey
    ON platform.user_history ((wamn_authority.tenant_key(tenant_id)));
CREATE TRIGGER wamn_record_history_log
    AFTER INSERT OR UPDATE OR DELETE ON platform."user"
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.log_row_change('unlimited');

ALTER TABLE platform.roles_history ENABLE ROW LEVEL SECURITY;
ALTER TABLE platform.roles_history FORCE ROW LEVEL SECURITY;
CREATE POLICY roles_history_tenant ON platform.roles_history
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY roles_history_platform ON platform.roles_history
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX roles_history_tkey
    ON platform.roles_history ((wamn_authority.tenant_key(tenant_id)));
CREATE TRIGGER wamn_record_history_log
    AFTER INSERT OR UPDATE OR DELETE ON platform.roles
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.log_row_change('unlimited');

ALTER TABLE platform.user_roles_history ENABLE ROW LEVEL SECURITY;
ALTER TABLE platform.user_roles_history FORCE ROW LEVEL SECURITY;
CREATE POLICY user_roles_history_tenant ON platform.user_roles_history
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY user_roles_history_platform ON platform.user_roles_history
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX user_roles_history_tkey
    ON platform.user_roles_history ((wamn_authority.tenant_key(tenant_id)));
CREATE TRIGGER wamn_record_history_log
    AFTER INSERT OR UPDATE OR DELETE ON platform.user_roles
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.log_row_change('unlimited');

ALTER TABLE platform.permissions_history ENABLE ROW LEVEL SECURITY;
ALTER TABLE platform.permissions_history FORCE ROW LEVEL SECURITY;
CREATE POLICY permissions_history_tenant ON platform.permissions_history
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY permissions_history_platform ON platform.permissions_history
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX permissions_history_tkey
    ON platform.permissions_history ((wamn_authority.tenant_key(tenant_id)));
CREATE TRIGGER wamn_record_history_log
    AFTER INSERT OR UPDATE OR DELETE ON platform.permissions
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.log_row_change('unlimited');

ALTER TABLE platform.api_keys_history ENABLE ROW LEVEL SECURITY;
ALTER TABLE platform.api_keys_history FORCE ROW LEVEL SECURITY;
CREATE POLICY api_keys_history_tenant ON platform.api_keys_history
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY api_keys_history_platform ON platform.api_keys_history
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX api_keys_history_tkey
    ON platform.api_keys_history ((wamn_authority.tenant_key(tenant_id)));
CREATE TRIGGER wamn_record_history_log
    AFTER INSERT OR UPDATE OR DELETE ON platform.api_keys
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.log_row_change('unlimited');

ALTER TABLE platform.environment_history ENABLE ROW LEVEL SECURITY;
ALTER TABLE platform.environment_history FORCE ROW LEVEL SECURITY;
CREATE POLICY environment_history_tenant ON platform.environment_history
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY environment_history_platform ON platform.environment_history
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX environment_history_tkey
    ON platform.environment_history ((wamn_authority.tenant_key(tenant_id)));
CREATE TRIGGER wamn_record_history_log
    AFTER INSERT OR UPDATE OR DELETE ON platform.environment
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.log_row_change('unlimited');

-- ---------------------------------------------------------------------------
-- THE WRITE LOG (`docs/plan/write-log.md`, wamn-z7uv). One idempotency record
-- for every create and command in this database. For an operation with SQL,
-- the engine inserts the claim in the item's host transaction, the guest does
-- the work in it, and the engine stores the result and commits, so the row
-- commits with its result. A refused item rolls its claim back, so it leaves
-- no row. An operation without SQL claims in two steps
-- (`docs/plan/host-transaction.md` 4.3): the claim commits with a null
-- `result`, the finish stores it, and a refused item deletes its claim. A
-- committed row with a null `result` is a claim that began and never
-- finished, which is uncertain.
--
-- The primary key is the operation and the key. `operation` is the contract
-- operation without its @version, so a retry across a release answers the
-- stored result, and two packages in one database do not collide.
--
-- The table is outside the tenant floor and the record history on purpose.
-- The database is the tenant, so it has no tenant_id and no row policy. The
-- rows that the work writes carry the actor, so it has no user columns and no
-- stamp or log trigger. wamn_app inserts a claim, reads a claim, writes the
-- result and deletes a refused two-step claim, and changes no other column.
--
-- IF NOT EXISTS, so that this section runs again on a database that has it.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS platform.write_log (
    operation       text NOT NULL CHECK (operation <> ''),
    idempotency_key text NOT NULL CHECK (idempotency_key <> ''),
    request         bytea NOT NULL CHECK (octet_length(request) > 0),
    result          text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT write_log_pkey PRIMARY KEY (operation, idempotency_key)
);
GRANT SELECT, INSERT ON platform.write_log TO wamn_app;
GRANT UPDATE (result) ON platform.write_log TO wamn_app;
GRANT DELETE ON platform.write_log TO wamn_app;

-- ---------------------------------------------------------------------------
-- PLATFORM CONFIGURATION (docs/plan/platform-config.md, Tables and Numbering).
-- The platform writes these rows through its routes and the host, and the
-- application reads them: wamn_app holds SELECT on each table and EXECUTE on
-- platform.bump_counter, and nothing else. An integration is connector code,
-- named here by its catalog code. A connection is a configured instance of an
-- integration. An external system is the persistent identity that a group of
-- connections reach. Credentials and targets stay in the control plane, so no
-- table here holds them.
--
-- Each table has the tenant floor and the record history of the tables above.
-- number_counter is the one exception: it has no history table and no log
-- trigger, because one history entry per bump would double the writes of
-- every create, and the write log is the audit (owner ruling, wamn-aqd0m.1).
--
-- Org and project ids are the text slugs of the system database, so scope_id
-- and project_id are text. A role scope names a role by name, and an
-- application scope names an application by name.
--
-- Project migration 0025 carries this section verbatim, and a test in
-- crates/control/provision/src/schema_migrations.rs compares the bytes.
-- ---------------------------------------------------------------------------
CREATE TABLE platform.external_system (
    tenant_id   text NOT NULL CHECK (tenant_id <> ''),
    id          uuid NOT NULL DEFAULT gen_random_uuid(),
    integration text NOT NULL CHECK (integration <> ''),
    name        text NOT NULL CHECK (name <> ''),
    parameters  jsonb NOT NULL DEFAULT '{}'::jsonb,
    status      text NOT NULL DEFAULT 'active',
    created_at  timestamptz NOT NULL,
    created_by  uuid NOT NULL,
    updated_at  timestamptz NOT NULL,
    updated_by  uuid NOT NULL,
    PRIMARY KEY (tenant_id, id),
    UNIQUE (tenant_id, name),
    CONSTRAINT external_system_status_check
        CHECK (status IN ('active', 'disabled'))
);

CREATE TABLE platform.connection (
    tenant_id          text NOT NULL CHECK (tenant_id <> ''),
    id                 uuid NOT NULL DEFAULT gen_random_uuid(),
    integration        text NOT NULL CHECK (integration <> ''),
    external_system_id uuid,
    name               text NOT NULL CHECK (name <> ''),
    transport          text NOT NULL CHECK (transport <> ''),
    exchange           text NOT NULL,
    parameters         jsonb NOT NULL DEFAULT '{}'::jsonb,
    status             text NOT NULL DEFAULT 'active',
    created_at         timestamptz NOT NULL,
    created_by         uuid NOT NULL,
    updated_at         timestamptz NOT NULL,
    updated_by         uuid NOT NULL,
    PRIMARY KEY (tenant_id, id),
    UNIQUE (tenant_id, name),
    FOREIGN KEY (tenant_id, external_system_id)
        REFERENCES platform.external_system (tenant_id, id),
    CONSTRAINT connection_exchange_check
        CHECK (exchange IN ('request', 'event')),
    CONSTRAINT connection_status_check
        CHECK (status IN ('active', 'disabled'))
);

-- The integrations allowed at one scope. Each scope narrows the one above it:
-- org, then project, then environment.
CREATE TABLE platform.integration_allow (
    tenant_id   text NOT NULL CHECK (tenant_id <> ''),
    scope_type  text NOT NULL,
    scope_id    text NOT NULL CHECK (scope_id <> ''),
    integration text NOT NULL CHECK (integration <> ''),
    created_at  timestamptz NOT NULL,
    created_by  uuid NOT NULL,
    updated_at  timestamptz NOT NULL,
    updated_by  uuid NOT NULL,
    PRIMARY KEY (tenant_id, scope_type, scope_id, integration),
    CONSTRAINT integration_allow_scope_type_check
        CHECK (scope_type IN ('org', 'project', 'environment'))
);

-- The connections a project has enabled.
CREATE TABLE platform.project_connection (
    tenant_id     text NOT NULL CHECK (tenant_id <> ''),
    project_id    text NOT NULL CHECK (project_id <> ''),
    connection_id uuid NOT NULL,
    created_at    timestamptz NOT NULL,
    created_by    uuid NOT NULL,
    updated_at    timestamptz NOT NULL,
    updated_by    uuid NOT NULL,
    PRIMARY KEY (tenant_id, project_id, connection_id),
    FOREIGN KEY (tenant_id, connection_id)
        REFERENCES platform.connection (tenant_id, id)
);

-- One document number sequence of one application, and its pattern.
CREATE TABLE platform.number_sequence (
    tenant_id   text NOT NULL CHECK (tenant_id <> ''),
    id          uuid NOT NULL DEFAULT gen_random_uuid(),
    application text NOT NULL CHECK (application <> ''),
    name        text NOT NULL CHECK (name <> ''),
    pattern     text NOT NULL CHECK (pattern <> ''),
    created_at  timestamptz NOT NULL,
    created_by  uuid NOT NULL,
    updated_at  timestamptz NOT NULL,
    updated_by  uuid NOT NULL,
    PRIMARY KEY (tenant_id, id),
    UNIQUE (tenant_id, application, name)
);

-- One counter of a sequence for one key value. `value` is the next value to
-- render. The '' key value is the allocator row of a per= counter.
CREATE TABLE platform.number_counter (
    tenant_id   text NOT NULL CHECK (tenant_id <> ''),
    sequence_id uuid NOT NULL,
    counter     text NOT NULL CHECK (counter <> ''),
    key_value   text NOT NULL,
    value       bigint NOT NULL CHECK (value >= 1),
    created_at  timestamptz NOT NULL,
    created_by  uuid NOT NULL,
    updated_at  timestamptz NOT NULL,
    updated_by  uuid NOT NULL,
    PRIMARY KEY (tenant_id, sequence_id, counter, key_value),
    FOREIGN KEY (tenant_id, sequence_id)
        REFERENCES platform.number_sequence (tenant_id, id) ON DELETE CASCADE
);

-- Display and view settings, resolved from least to most specific scope. A
-- locked row stops overrides at more specific scopes.
CREATE TABLE platform.setting (
    tenant_id  text NOT NULL CHECK (tenant_id <> ''),
    scope_type text NOT NULL,
    scope_id   text NOT NULL CHECK (scope_id <> ''),
    key        text NOT NULL CHECK (key <> ''),
    value      jsonb NOT NULL,
    locked     boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (tenant_id, scope_type, scope_id, key),
    CONSTRAINT setting_scope_type_check
        CHECK (scope_type IN ('org', 'project', 'application', 'role', 'user'))
);

-- The tenant floor and the stamp trigger of each table, and the SELECT of
-- wamn_app.
DO $platform_configuration$
DECLARE relation text;
BEGIN
  FOREACH relation IN ARRAY ARRAY['external_system', 'connection', 'integration_allow',
                                  'project_connection', 'number_sequence',
                                  'number_counter', 'setting'] LOOP
    EXECUTE format('CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON platform.%I '
                   'FOR EACH ROW EXECUTE FUNCTION '
                   'wamn_history.stamp_row(''created_at'', ''created_by'', ''updated_at'', ''updated_by'')',
                   relation);
    EXECUTE format('ALTER TABLE platform.%I ENABLE ROW LEVEL SECURITY', relation);
    EXECUTE format('ALTER TABLE platform.%I FORCE ROW LEVEL SECURITY', relation);
    EXECUTE format('CREATE POLICY %I ON platform.%I TO wamn_app '
                   'USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()) '
                   'WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())',
                   relation || '_tenant', relation);
    EXECUTE format('CREATE POLICY %I ON platform.%I AS PERMISSIVE FOR ALL TO wamn_platform '
                   'USING (true) WITH CHECK (true)',
                   relation || '_platform', relation);
    EXECUTE format('CREATE INDEX %I ON platform.%I ((wamn_authority.tenant_key(tenant_id)))',
                   relation || '_tkey', relation);
    EXECUTE format('GRANT SELECT ON platform.%I TO wamn_app', relation);
  END LOOP;
END $platform_configuration$;

-- The history table of each table except number_counter, with the tenant
-- floor of its base table, as in HISTORY TABLES above.
DO $platform_configuration_history$
DECLARE relation text;
BEGIN
  FOREACH relation IN ARRAY ARRAY['external_system', 'connection', 'integration_allow',
                                  'project_connection', 'number_sequence', 'setting'] LOOP
    PERFORM wamn_history.create_history_table('platform', relation, true);
    EXECUTE format('ALTER TABLE platform.%I ENABLE ROW LEVEL SECURITY', relation || '_history');
    EXECUTE format('ALTER TABLE platform.%I FORCE ROW LEVEL SECURITY', relation || '_history');
    EXECUTE format('CREATE POLICY %I ON platform.%I TO wamn_app '
                   'USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()) '
                   'WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())',
                   relation || '_history_tenant', relation || '_history');
    EXECUTE format('CREATE POLICY %I ON platform.%I AS PERMISSIVE FOR ALL TO wamn_platform '
                   'USING (true) WITH CHECK (true)',
                   relation || '_history_platform', relation || '_history');
    EXECUTE format('CREATE INDEX %I ON platform.%I ((wamn_authority.tenant_key(tenant_id)))',
                   relation || '_history_tkey', relation || '_history');
    EXECUTE format('CREATE TRIGGER wamn_record_history_log AFTER INSERT OR UPDATE OR DELETE '
                   'ON platform.%I FOR EACH ROW EXECUTE FUNCTION '
                   'wamn_history.log_row_change(''unlimited'')',
                   relation);
  END LOOP;
END $platform_configuration_history$;

-- bump_counter advances one counter and returns the value to render: it locks
-- the counter row, inserts it at 1 when it is absent, returns its value and
-- increments it. It knows no pattern and parses nothing; the renderer of the
-- wamn:postgres plugin owns both. It runs as its owner, the applier, so that
-- wamn_app needs no write on number_counter.
--
-- The owner's authority would read every tenant, so the function takes the
-- caller's tenant from the caller's role name, with the parse of
-- wamn_authority.current_tenant_key(): the SET ROLE role, or the session user
-- when the session set no role. A definer function keeps the role setting of
-- its caller. A sequence of another tenant, or a caller that is no App
-- generation, is refused with insufficient_privilege.
CREATE FUNCTION platform.bump_counter(target_sequence uuid, target_counter text, target_key text)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
DECLARE
    caller_key text := substring(COALESCE(NULLIF(current_setting('role'), 'none'), session_user::text)
                                 from '^wamn_app_([0-9a-f]{40})_[ab]$');
    sequence_tenant text;
    allocated bigint;
BEGIN
    SELECT s.tenant_id INTO sequence_tenant
      FROM platform.number_sequence AS s
     WHERE s.id = target_sequence
       AND wamn_authority.tenant_key(s.tenant_id) = caller_key;
    IF sequence_tenant IS NULL THEN
        RAISE EXCEPTION 'number sequence % is not a sequence of the caller''s tenant', target_sequence
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    INSERT INTO platform.number_counter AS c (tenant_id, sequence_id, counter, key_value, value)
    VALUES (sequence_tenant, target_sequence, target_counter, target_key, 2)
    ON CONFLICT (tenant_id, sequence_id, counter, key_value)
    DO UPDATE SET value = c.value + 1
    RETURNING c.value - 1 INTO allocated;
    RETURN allocated;
END;
$$;
REVOKE ALL ON FUNCTION platform.bump_counter(uuid, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.bump_counter(uuid, text, text) TO wamn_app;
