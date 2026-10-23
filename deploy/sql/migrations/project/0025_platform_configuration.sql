-- The platform configuration tables (docs/plan/platform-config.md, C1.1) in an
-- installed project database. Owner rulings of wamn-aqd0m.1:
--
-- - platform.configurations and its history table are removed with their
--   grants. platform.setting replaces them.
-- - wamn_app loses SELECT on platform.api_keys, which no runtime code reads.
--   The table stays.
-- - The rest is the PLATFORM CONFIGURATION section of deploy/sql/app-schema.sql,
--   byte for byte. A test in crates/control/provision/src/schema_migrations.rs
--   compares the bytes. A fresh install records this file as applied, because
--   app-schema.sql already holds it.

DROP TABLE platform.configurations_history;
DROP TABLE platform.configurations;
REVOKE ALL PRIVILEGES ON TABLE platform.api_keys FROM wamn_app;

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
