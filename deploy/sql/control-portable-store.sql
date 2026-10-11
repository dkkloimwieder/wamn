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
    tenant_id            text        NOT NULL CHECK (tenant_id <> ''),
    org                  text        NOT NULL CHECK (org <> ''),
    project              text        NOT NULL CHECK (project <> ''),
    env                  text        NOT NULL CHECK (env <> ''),
    instance_suffix      text        NOT NULL CHECK (instance_suffix ~ '^[a-z0-9]{8}$'),
    disposable           boolean     NOT NULL,
    environment_instance text        NOT NULL,
    projected_at         timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT tenant_environments_pkey PRIMARY KEY (tenant_id)
);

-- `principal_type` admits `human`, because the audit is immutable and rows
-- written before system migration 0007 keep the old name of `user`.
CREATE TABLE catalog.authoring_command_audit (
    tenant_id         text        NOT NULL CHECK (tenant_id <> ''),
    audit_id          uuid        NOT NULL DEFAULT gen_random_uuid(),
    command_id        text        NOT NULL CHECK (command_id <> ''),
    command_type      text        NOT NULL CHECK (command_type IN ('gate', 'publish')),
    principal_id      text        NOT NULL CHECK (principal_id <> ''),
    principal_type    text        NOT NULL CHECK (principal_type IN ('user', 'service', 'human')),
    principal_subject text        NOT NULL CHECK (principal_subject <> ''),
    effective_role    text        NOT NULL
        CHECK (effective_role IN ('project-author', 'project-admin')),
    org               text        NOT NULL CHECK (org <> ''),
    project           text        NOT NULL CHECK (project <> ''),
    environment       text        NOT NULL CHECK (environment <> ''),
    target_ref        text        NOT NULL CHECK (target_ref <> ''),
    request_hash      text        NOT NULL CHECK (request_hash ~ '^sha256:[0-9a-f]{64}$'),
    outcome_bytes     bytea       NOT NULL CHECK (octet_length(outcome_bytes) > 0),
    provenance_commit text,
    provenance_ref    text,
    provenance_dirty  boolean,
    recorded_at       timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT authoring_command_audit_pkey
        PRIMARY KEY (tenant_id, principal_id, command_id),
    CONSTRAINT authoring_command_audit_id_key UNIQUE (tenant_id, audit_id),
    CONSTRAINT authoring_command_audit_provenance_check CHECK (
        (provenance_commit IS NULL) = (provenance_dirty IS NULL)
        AND (provenance_commit IS NULL OR provenance_commit <> '')
        AND (provenance_ref IS NULL OR provenance_ref <> '')
        AND (provenance_commit IS NOT NULL OR provenance_ref IS NULL)
    )
);
CREATE INDEX authoring_command_audit_recorded
    ON catalog.authoring_command_audit (tenant_id, recorded_at);

CREATE TABLE wamn_run.gate_reports (
    tenant_id   text        NOT NULL CHECK (tenant_id <> ''),
    wiring_hash text        NOT NULL CHECK (wiring_hash ~ '^sha256:[0-9a-f]{64}$'),
    passed      boolean     NOT NULL,
    summary     jsonb       NOT NULL CHECK (jsonb_typeof(summary) = 'object'),
    gated_at    timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT gate_reports_pkey PRIMARY KEY (tenant_id, wiring_hash)
);

-- Tenant isolation is structural even for owner-only tables.
DO $tenant_policies$
DECLARE
    relation_name text;
    policy_name text;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'catalog.authoring_command_audit',
        'catalog.tenant_environments',
        'wamn_run.gate_reports'
    ] LOOP
        EXECUTE format('ALTER TABLE %s ENABLE ROW LEVEL SECURITY', relation_name);
        EXECUTE format('ALTER TABLE %s FORCE ROW LEVEL SECURITY', relation_name);
        policy_name := split_part(relation_name, '.', 2) || '_tenant';
        EXECUTE format(
            'CREATE POLICY %I ON %s USING (tenant_id = NULLIF(current_setting(''app.tenant'', true), '''')) WITH CHECK (tenant_id = NULLIF(current_setting(''app.tenant'', true), ''''))',
            policy_name, relation_name
        );
    END LOOP;
END
$tenant_policies$;

DO $immutable_facts$
DECLARE
    relation_name text;
    trigger_name text;
BEGIN
    -- ONE freeze for every control fact, unconditionally (wamn-10yt.52).
    FOREACH relation_name IN ARRAY ARRAY[
        'catalog.authoring_command_audit',
        'wamn_run.gate_reports'
    ] LOOP
        trigger_name := split_part(relation_name, '.', 2) || '_immutable';
        EXECUTE format(
            'CREATE TRIGGER %I BEFORE UPDATE OR DELETE ON %s FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change()',
            trigger_name, relation_name
        );
    END LOOP;
END
$immutable_facts$;

-- The control author is mapped to one tenant by its exact login generation.
CREATE SCHEMA IF NOT EXISTS wamn_authority AUTHORIZATION wamn_system;
REVOKE ALL ON SCHEMA wamn_authority FROM PUBLIC;

CREATE TABLE wamn_authority.author_login_tenants (
    login_identity text        NOT NULL CHECK (login_identity <> ''),
    tenant_id      text        NOT NULL CHECK (tenant_id <> ''),
    org_id         text        NOT NULL CHECK (org_id <> ''),
    project_id     text        NOT NULL CHECK (project_id <> ''),
    environment    text        NOT NULL CHECK (environment <> ''),
    created_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT author_login_tenants_pkey PRIMARY KEY (login_identity)
);
REVOKE ALL ON TABLE wamn_authority.author_login_tenants FROM PUBLIC;

CREATE OR REPLACE FUNCTION wamn_authority.session_author_tenant()
RETURNS text
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, wamn_authority
AS $$
    SELECT mapping.tenant_id
      FROM wamn_authority.author_login_tenants AS mapping
     WHERE mapping.login_identity = session_user
$$;
REVOKE ALL ON FUNCTION wamn_authority.session_author_tenant() FROM PUBLIC;

DO $author_policies$
DECLARE
    relation_name text;
    policy_name text;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'catalog.authoring_command_audit',
        'wamn_run.gate_reports'
    ] LOOP
        policy_name := split_part(relation_name, '.', 2) || '_author_tenant';
        EXECUTE format(
            'CREATE POLICY %I ON %s AS RESTRICTIVE TO wamn_control_author USING (tenant_id = wamn_authority.session_author_tenant()) WITH CHECK (tenant_id = wamn_authority.session_author_tenant())',
            policy_name, relation_name
        );
    END LOOP;
END
$author_policies$;

REVOKE ALL PRIVILEGES ON SCHEMA catalog, wamn_run, wamn_authority
    FROM wamn_control_author;
GRANT USAGE ON SCHEMA catalog, wamn_run, wamn_authority TO wamn_control_author;
GRANT EXECUTE ON FUNCTION wamn_authority.session_author_tenant()
    TO wamn_control_author;

REVOKE ALL PRIVILEGES ON catalog.authoring_command_audit, wamn_run.gate_reports
    FROM wamn_control_author;
GRANT SELECT, INSERT ON catalog.authoring_command_audit, wamn_run.gate_reports
    TO wamn_control_author;

REVOKE ALL ON ALL TABLES IN SCHEMA catalog, wamn_run FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA catalog FROM PUBLIC;

-- Assert the server objects produced by this artifact. There is deliberately
-- no checked-in fingerprint or compatibility list to regenerate.
DO $objects$
DECLARE
    catalog_tables text[];
    run_tables text[];
    unexpected text;
BEGIN
    SELECT array_agg(tablename ORDER BY tablename) INTO catalog_tables
      FROM pg_tables WHERE schemaname = 'catalog';
    IF catalog_tables IS DISTINCT FROM ARRAY[
        'authoring_command_audit', 'tenant_environments'
    ]::text[] THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'control-portable-catalog-inventory-drift';
    END IF;

    SELECT array_agg(tablename ORDER BY tablename) INTO run_tables
      FROM pg_tables WHERE schemaname = 'wamn_run';
    IF run_tables IS DISTINCT FROM ARRAY['gate_reports']::text[] THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'control-portable-run-inventory-drift';
    END IF;

    SELECT string_agg(format('%s:%s', relation, privilege), ', '
                      ORDER BY relation, privilege)
      INTO unexpected
      FROM (
        SELECT quote_ident(namespace.nspname) || '.' || quote_ident(relation.relname)
                   AS relation,
               candidate.privilege
          FROM pg_class AS relation
          JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
          CROSS JOIN unnest(ARRAY['SELECT', 'INSERT', 'UPDATE', 'DELETE',
                                  'TRUNCATE', 'REFERENCES', 'TRIGGER'])
               AS candidate(privilege)
         WHERE relation.relkind IN ('r', 'p')
           AND namespace.nspname IN ('catalog', 'wamn_run', 'wamn_authority')
           AND has_table_privilege('wamn_control_author', relation.oid,
                                   candidate.privilege)
           AND NOT EXISTS (
             SELECT 1 FROM (VALUES
                 ('catalog', 'authoring_command_audit', 'SELECT'),
                 ('catalog', 'authoring_command_audit', 'INSERT'),
                 ('wamn_run', 'gate_reports', 'SELECT'),
                 ('wamn_run', 'gate_reports', 'INSERT')
             ) AS allowed(schema_name, table_name, privilege)
             WHERE allowed.schema_name = namespace.nspname
               AND allowed.table_name = relation.relname
               AND allowed.privilege = candidate.privilege
           )
      ) AS excess;
    IF unexpected IS NOT NULL THEN
        RAISE EXCEPTION USING ERRCODE = '42501',
            MESSAGE = 'control-author-effective-privilege-out-of-bounds',
            DETAIL = unexpected;
    END IF;
END
$objects$;
