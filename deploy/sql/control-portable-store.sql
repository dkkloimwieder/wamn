-- Environment declarations and scoped reader authority in the control database.
-- Apply after system-schema.sql as wamn_system.
CREATE SCHEMA IF NOT EXISTS catalog AUTHORIZATION wamn_system;
REVOKE ALL ON SCHEMA catalog FROM PUBLIC;

-- ---------------------------------------------------------------------------
-- The environment identity behind a tenant's control facts, PROJECTED from
-- `registry.project_envs` (wamn-10yt.38).
--
-- `registry.project_envs` is the AUTHORITY for `disposable`; this is one
-- projection of that one source, written by provisioning into the plane that
-- holds the facts the marker governs. The admit path reads it LOCALLY, inside
-- its own control transaction, rather than opening the registry on every
-- component admission for a flag that changes once in an environment's
-- lifetime.
--
-- The projected copy CARRIES THE AUTHORITY ROW'S IDENTITY — the (org, project,
-- env) triple plus the provisioned `instance_suffix` — so a disagreement
-- between the two planes is DETECTABLE rather than silent:
-- The Rust provisioning owner refuses a second triple under one
-- tenant instead of overwriting the first.
--
-- ABSENCE MEANS DURABLE. A tenant with no row here is frozen exactly as before,
-- so the projection is additive: a control store that never sees a disposable
-- environment behaves as it always did.
--
-- `environment_instance` is the tenant's CURRENT creation of the project
-- database, and it is what every fact above keys on (wamn-10yt.52). Provisioning
-- projects it EMPTY, because provisioning does not recreate anything; the
-- development loop's recreate then claims the instance it just minted through
-- the Rust development coordinator.
--
-- `disposable` STAYS, though the three overwrites it used to gate are gone. It
-- is still read: `publish_release` selects wamn-10yt.48's dependency-digest rule
-- from it.
--
-- RETENTION (wamn-10yt.70). Rows keyed by a non-empty `environment_instance`
-- accumulate only within one `wamn dev up` standup. Nothing deletes them one at
-- a time. That command's control store reset drops and installs this whole
-- store again, and it is the retention step, outside the recreate path. The
-- immutability trigger below stays unconditional.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.tenant_environments (
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    env_key              text        NOT NULL CHECK (env_key <> ''),
    org                  text        NOT NULL CHECK (org <> ''),
    project              text        NOT NULL CHECK (project <> ''),
    env                  text        NOT NULL CHECK (env <> ''),
    instance_suffix      text        NOT NULL CHECK (instance_suffix ~ '^[a-z0-9]{8}$'),
    disposable           boolean     NOT NULL,
    environment_instance text        NOT NULL,
    projected_at         timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT tenant_environments_pkey PRIMARY KEY (env_key)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON catalog.tenant_environments
    FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row(
        'created_at', 'created_by', 'updated_at', 'updated_by');

-- Tenant isolation is structural even for owner-only tables.
DO $tenant_policies$
DECLARE
    relation_name text;
    policy_name text;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'catalog.tenant_environments'
    ] LOOP
        EXECUTE format('ALTER TABLE %s ENABLE ROW LEVEL SECURITY', relation_name);
        EXECUTE format('ALTER TABLE %s FORCE ROW LEVEL SECURITY', relation_name);
        policy_name := split_part(relation_name, '.', 2) || '_tenant';
        EXECUTE format(
            'CREATE POLICY %I ON %s USING (env_key = NULLIF(current_setting(''app.env_key'', true), '''')) WITH CHECK (env_key = NULLIF(current_setting(''app.env_key'', true), ''''))',
            policy_name, relation_name
        );
    END LOOP;
END
$tenant_policies$;

-- The control author is mapped to one tenant by its exact login generation.
CREATE SCHEMA IF NOT EXISTS wamn_authority AUTHORIZATION wamn_system;
REVOKE ALL ON SCHEMA wamn_authority FROM PUBLIC;

CREATE TABLE wamn_authority.author_login_tenants (
    login_identity text        NOT NULL CHECK (login_identity <> ''),
    env_key        text        NOT NULL CHECK (env_key <> ''),
    org_id         text        NOT NULL CHECK (org_id <> ''),
    project_id     text        NOT NULL CHECK (project_id <> ''),
    environment    text        NOT NULL CHECK (environment <> ''),
    created_at     timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    CONSTRAINT author_login_tenants_pkey PRIMARY KEY (login_identity)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON wamn_authority.author_login_tenants
    FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row(
        'created_at', 'created_by', 'updated_at', 'updated_by');
REVOKE ALL ON TABLE wamn_authority.author_login_tenants FROM PUBLIC;

REVOKE ALL ON ALL TABLES IN SCHEMA catalog FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA catalog FROM PUBLIC;
