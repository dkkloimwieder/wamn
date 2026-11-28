-- Package and release facts carry the four NOT NULL record history stamps.
-- Their triggers use wamn_history.stamp_row(), installed by CATALOG_SCHEMA_SQL.
-- Package application binds wamn:apply-package. Connection and workflow
-- administration bind the caller and the sealed operation. The history tables
-- below retain row changes for unlimited time.
-- app-schema.sql adds the stamp keys after it seeds the project principals.

CREATE TABLE catalog.package (
    package_id         text        NOT NULL,
    package_version    text        NOT NULL,
    registered_at      timestamptz NOT NULL DEFAULT now(),
    -- Current SQL source, independent of executable release selection.
    -- NULL: wamn dev applied a working tree.
    artifact_digest    text,
    created_at         timestamptz NOT NULL,
    created_by         uuid        NOT NULL,
    updated_at         timestamptz NOT NULL,
    updated_by         uuid        NOT NULL,
    CONSTRAINT package_pkey PRIMARY KEY (package_id),
    CONSTRAINT package_package_id_check
        CHECK (package_id ~ '^[a-z][a-z0-9]*(_[a-z0-9]+)*$'),
    CONSTRAINT package_package_version_check CHECK (
        package_version <> ''
        AND package_version = btrim(package_version)
        AND strpos(package_version, '@') = 0
        AND strpos(package_version, '::') = 0
    ),
    CONSTRAINT package_artifact_digest_check
        CHECK (artifact_digest IS NULL OR artifact_digest ~ '^sha256:[0-9a-f]{64}$')
);

CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON catalog.package
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- The database instance owns these records. Replacing it starts a new baseline.
CREATE FUNCTION catalog.guard_package_schema()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF ROW(NEW.package_id, NEW.registered_at)
       IS DISTINCT FROM ROW(OLD.package_id, OLD.registered_at) THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-baseline-immutable';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER package_schema_guard
    BEFORE UPDATE ON catalog.package
    FOR EACH ROW EXECUTE FUNCTION catalog.guard_package_schema();
CREATE TRIGGER package_delete_immutable
    BEFORE DELETE ON catalog.package
    FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();


-- Project-owned connections precede the catalog references to them.
CREATE SCHEMA IF NOT EXISTS platform AUTHORIZATION CURRENT_USER;

CREATE TABLE platform.external_system (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    name text NOT NULL UNIQUE CHECK (name ~ '^[a-z][a-z0-9_-]{0,62}$')
);

CREATE TABLE platform.connection (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    name text NOT NULL UNIQUE CHECK (name ~ '^[a-z][a-z0-9_-]{0,62}$'),
    integration_type text NOT NULL,
    auth_type text NOT NULL,
    external_system_id uuid REFERENCES platform.external_system (id),
    target_id uuid NOT NULL,
    resource_key text,
    authority text CHECK (authority IS NULL OR authority <> ''),
    active_generation bigint NOT NULL,
    service_principal_id uuid NOT NULL UNIQUE,
    active boolean NOT NULL DEFAULT true
);
CREATE UNIQUE INDEX connection_resource_per_target
    ON platform.connection (target_id, resource_key)
    WHERE resource_key IS NOT NULL;

CREATE TABLE platform.connection_event_type (
    code text PRIMARY KEY,
    label text NOT NULL
);
INSERT INTO platform.connection_event_type VALUES
    ('activate', 'Activate'),
    ('deactivate', 'Deactivate'),
    ('credential_change', 'Credential change'),
    ('oauth_authorize', 'OAuth authorize'),
    ('oauth_callback', 'OAuth callback'),
    ('token_refresh', 'OAuth token refresh');

CREATE TABLE platform.connection_event_outcome (
    code text PRIMARY KEY,
    label text NOT NULL,
    success boolean NOT NULL
);
INSERT INTO platform.connection_event_outcome VALUES
    ('succeeded', 'Succeeded', true),
    ('refused', 'Refused', false),
    ('failed', 'Failed', false),
    ('superseded', 'Connection generation changed', false);

CREATE TABLE platform.connection_event (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    connection_id uuid NOT NULL REFERENCES platform.connection (id) ON DELETE RESTRICT,
    generation bigint NOT NULL,
    principal_id uuid NOT NULL,
    occurred_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    event_type text NOT NULL REFERENCES platform.connection_event_type (code),
    outcome text NOT NULL REFERENCES platform.connection_event_outcome (code)
);
CREATE INDEX connection_event_timeline
    ON platform.connection_event (connection_id, occurred_at DESC, id DESC);
CREATE INDEX connection_event_latest_refresh
    ON platform.connection_event (connection_id, occurred_at DESC, id DESC) INCLUDE (generation)
    WHERE event_type = 'token_refresh';

CREATE TRIGGER connection_event_immutable
    BEFORE UPDATE OR DELETE OR TRUNCATE ON platform.connection_event
    FOR EACH STATEMENT EXECUTE FUNCTION catalog.reject_immutable_row_change();

CREATE TABLE platform.credential (
    id            uuid PRIMARY KEY DEFAULT uuidv7(),
    connection_id uuid NOT NULL REFERENCES platform.connection (id) ON DELETE RESTRICT,
    store         text NOT NULL CHECK (store = 'k8s'),
    ref           text NOT NULL,
    version       text NOT NULL,
    created_at    timestamptz NOT NULL,
    created_by    uuid NOT NULL,
    UNIQUE (id, connection_id)
);
CREATE TABLE platform.oauth_token (                    -- host-only; every OAuth type, shared across pods
    connection_id  uuid PRIMARY KEY REFERENCES platform.connection (id) ON DELETE RESTRICT,
    ciphertext     bytea NOT NULL,                       -- AES-256-GCM: access token; refresh token for authorization code
    nonce          bytea NOT NULL,
    wrapped_key    bytea NOT NULL,                       -- the row's AES-256 data key, wrapped by the envelope key
    key_id         text NOT NULL,                        -- the envelope key: a Cloud KMS key version (local: key file id)
    expires_at     timestamptz NOT NULL,
    updated_at     timestamptz NOT NULL,
    refresh_lease_until timestamptz NOT NULL DEFAULT '-infinity'
);
-- A lease can create a placeholder with updated_at = '-infinity' before the first token exists.
-- Authorization-code calls require a published token. Client credentials can fetch the first token.
-- Callback and refresh commit a lease and release the connection before provider I/O.
-- Short SQL transactions check the generation and lease before publishing a token.
--   client credentials: delete this row when the credential or parameters.auth changes
--   authorization code: delete this row when parameters.auth changes (authorize again);
--                       set expires_at = '-infinity' when only the secret changes (the refresh token stays)

CREATE TABLE platform.oauth_authorization (            -- one pending connection.authorize; single use
    state         text PRIMARY KEY,                      -- project/env/32 random bytes, base64url
    connection_id uuid NOT NULL REFERENCES platform.connection (id) ON DELETE RESTRICT,
    generation    bigint NOT NULL,                       -- a newer active generation refuses the callback
    code_verifier bytea NOT NULL,                        -- PKCE, encrypted like the token (wrapped_key, nonce, key_id below)
    wrapped_key   bytea NOT NULL,
    nonce         bytea NOT NULL,
    key_id        text NOT NULL,
    created_by    uuid NOT NULL,
    expires_at    timestamptz NOT NULL                   -- 10 minutes after creation
);

CREATE TABLE platform.connection_generation (
    connection_id uuid NOT NULL REFERENCES platform.connection (id) ON DELETE RESTRICT,
    generation    bigint NOT NULL CHECK (generation > 0),
    parameters    jsonb NOT NULL DEFAULT '{}',
    credential_id uuid,
    created_at    timestamptz NOT NULL,
    created_by    uuid NOT NULL,
    PRIMARY KEY (connection_id, generation),
    FOREIGN KEY (credential_id, connection_id)
        REFERENCES platform.credential (id, connection_id)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT ON platform.credential
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by');
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT ON platform.connection_generation
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by');
ALTER TABLE platform.connection ADD FOREIGN KEY (id, active_generation)
    REFERENCES platform.connection_generation (connection_id, generation) DEFERRABLE INITIALLY DEFERRED;

CREATE FUNCTION platform.reject_connection_generation_change() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    IF TG_OP = 'DELETE' AND NOT EXISTS (
        SELECT FROM platform.connection WHERE id = OLD.connection_id
    ) THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'connection_generation_is_immutable' USING ERRCODE = '55000';
END $$;
REVOKE ALL ON FUNCTION platform.reject_connection_generation_change() FROM PUBLIC;
CREATE TRIGGER connection_generation_immutable BEFORE UPDATE OR DELETE ON platform.connection_generation
    FOR EACH ROW EXECUTE FUNCTION platform.reject_connection_generation_change();
CREATE TRIGGER credential_immutable BEFORE UPDATE OR DELETE ON platform.credential
    FOR EACH ROW EXECUTE FUNCTION platform.reject_connection_generation_change();

CREATE FUNCTION platform.keep_active_generation() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    IF NEW.active_generation < OLD.active_generation THEN
        RAISE EXCEPTION 'active_generation_cannot_decrease' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION platform.keep_active_generation() FROM PUBLIC;
CREATE TRIGGER connection_generation_forward BEFORE UPDATE OF active_generation ON platform.connection
    FOR EACH ROW EXECUTE FUNCTION platform.keep_active_generation();

CREATE FUNCTION platform.keep_connection_identity() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    IF ROW(NEW.name, NEW.integration_type, NEW.auth_type, NEW.authority) IS DISTINCT FROM
       ROW(OLD.name, OLD.integration_type, OLD.auth_type, OLD.authority) THEN
        RAISE EXCEPTION 'connection_identity_is_immutable' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION platform.keep_connection_identity() FROM PUBLIC;
CREATE TRIGGER connection_identity_immutable BEFORE UPDATE ON platform.connection
    FOR EACH ROW EXECUTE FUNCTION platform.keep_connection_identity();

CREATE FUNCTION platform.keep_external_system_name() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    IF NEW.name IS DISTINCT FROM OLD.name THEN
        RAISE EXCEPTION 'name_is_immutable' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION platform.keep_external_system_name() FROM PUBLIC;
CREATE TRIGGER external_system_name_immutable BEFORE UPDATE OF name ON platform.external_system
    FOR EACH ROW EXECUTE FUNCTION platform.keep_external_system_name();

GRANT USAGE ON SCHEMA platform TO wamn_app;
GRANT SELECT ON platform.external_system, platform.connection, platform.connection_generation TO wamn_app;
REVOKE ALL ON platform.credential, platform.oauth_token, platform.oauth_authorization FROM wamn_app, wamn_query;
REVOKE ALL ON platform.connection_event, platform.connection_event_type,
    platform.connection_event_outcome FROM wamn_app, wamn_query;
GRANT SELECT ON platform.connection_event, platform.connection_event_type,
    platform.connection_event_outcome TO wamn_platform;
GRANT INSERT ON platform.connection_event TO wamn_platform;
GRANT SELECT, INSERT, UPDATE, DELETE ON platform.external_system,
    platform.oauth_token, platform.oauth_authorization TO wamn_platform;
GRANT SELECT, INSERT, UPDATE ON platform.connection TO wamn_platform;
GRANT SELECT, INSERT, UPDATE ON platform.connection_generation, platform.credential TO wamn_platform;

CREATE TABLE catalog.connection_requirement (
    component_digest text NOT NULL CHECK (component_digest ~ '^sha256:[0-9a-f]{64}$'),
    package_id text NOT NULL,
    name text NOT NULL CHECK (name ~ '^[a-z][a-z0-9-]{0,62}$'),
    interface text NOT NULL,
    mode text NOT NULL CHECK (mode IN ('read', 'write')),
    PRIMARY KEY (component_digest, package_id, name)
);

-- One version of a workflow. Authoring derives immutable trigger columns
-- from the graph. Enable records a new WAL boundary.
CREATE TABLE catalog.workflow (
    env            text        NOT NULL CHECK (env <> ''),
    app            text        NOT NULL CHECK (app <> ''),
    workflow_id    text        NOT NULL CHECK (workflow_id <> ''),
    version        int         NOT NULL CHECK (version > 0),
    graph_json     jsonb       NOT NULL,
    graph_hash    text        NOT NULL CHECK (graph_hash ~ '^sha256:[0-9a-f]{64}$'),
    trigger        text        NOT NULL CHECK (trigger IN ('request', 'schedule', 'data_change')),
    path           text        CHECK (path LIKE '/%'),
    -- The five-field cron, evaluated in UTC, of a schedule trigger.
    schedule       text,
    source_package text,
    entity         text,
    ops            text[],
    condition      text,
    enable_lsn     pg_lsn,
    execution_type text        NOT NULL CHECK (execution_type IN ('role', 'invoker')),
    execution_role text        CHECK (execution_role <> ''),
    enabled        boolean     NOT NULL DEFAULT false,
    created_by     uuid        NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT workflow_pkey
        PRIMARY KEY (env, app, workflow_id, version),
    CONSTRAINT workflow_execution_identity_check
        CHECK ((execution_type = 'role') = (execution_role IS NOT NULL)
           AND (trigger = 'request' OR execution_role IS NOT NULL)),
    CONSTRAINT workflow_request_path_check
        CHECK ((trigger = 'request') = (path IS NOT NULL)),
    CONSTRAINT workflow_enable_lsn_check
        CHECK (NOT enabled OR enable_lsn IS NOT NULL),
    CONSTRAINT workflow_schedule_check
        CHECK ((trigger = 'schedule') = (schedule IS NOT NULL)),
    CONSTRAINT workflow_data_change_check CHECK (
        (trigger = 'data_change' AND source_package IS NOT NULL AND source_package <> ''
         AND entity IS NOT NULL AND entity <> '' AND ops IS NOT NULL
         AND cardinality(ops) BETWEEN 1 AND 3
         AND ops <@ ARRAY['insert', 'update', 'delete']::text[]
         AND array_position(ops, NULL) IS NULL
         AND cardinality(array_positions(ops, 'insert')) <= 1
         AND cardinality(array_positions(ops, 'update')) <= 1
         AND cardinality(array_positions(ops, 'delete')) <= 1
         AND (condition IS NULL OR btrim(condition) <> ''))
        OR (trigger <> 'data_change' AND source_package IS NULL AND entity IS NULL
            AND ops IS NULL AND condition IS NULL))
);
CREATE UNIQUE INDEX workflow_one_enabled
    ON catalog.workflow (env, app, workflow_id)
    WHERE enabled;
CREATE UNIQUE INDEX workflow_enabled_request_path
    ON catalog.workflow (env, app, path)
    WHERE enabled AND path IS NOT NULL;

CREATE FUNCTION catalog.guard_workflow_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF ROW(NEW.env, NEW.app, NEW.workflow_id,
           NEW.version, NEW.graph_json, NEW.graph_hash, NEW.trigger, NEW.path, NEW.schedule,
           NEW.source_package, NEW.entity, NEW.ops, NEW.condition,
           NEW.execution_type, NEW.execution_role, NEW.created_by, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.env, OLD.app, OLD.workflow_id,
           OLD.version, OLD.graph_json, OLD.graph_hash, OLD.trigger, OLD.path, OLD.schedule,
           OLD.source_package, OLD.entity, OLD.ops, OLD.condition,
           OLD.execution_type, OLD.execution_role, OLD.created_by, OLD.created_at) THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'workflow-version-immutable';
    END IF;
    IF NEW.enable_lsn IS DISTINCT FROM OLD.enable_lsn AND NOT NEW.enabled THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'workflow-enable-lsn-requires-enable';
    END IF;
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION catalog.guard_workflow_update() FROM PUBLIC;
CREATE TRIGGER workflow_controlled_update
    BEFORE UPDATE ON catalog.workflow
    FOR EACH ROW EXECUTE FUNCTION catalog.guard_workflow_update();
CREATE TRIGGER workflow_delete_immutable
    BEFORE DELETE ON catalog.workflow
    FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

-- Project base grants are one mechanism applied to the complete current relation
-- set. The server catalog, rather than tracked SQL text, shows the result.
DO $project_base_grants$
DECLARE
    relation_name text;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'package',
        'connection_requirement',
        'workflow'
    ] LOOP
        EXECUTE format('ALTER TABLE catalog.%I ENABLE ROW LEVEL SECURITY', relation_name);
        EXECUTE format('ALTER TABLE catalog.%I FORCE ROW LEVEL SECURITY', relation_name);
        EXECUTE format(
            'CREATE POLICY %I ON catalog.%I TO wamn_app, wamn_query USING ((SELECT wamn_authority.current_role()) IS NOT NULL) WITH CHECK ((SELECT wamn_authority.current_role()) IS NOT NULL)',
            relation_name || '_identity', relation_name
        );
        EXECUTE format(
            'CREATE POLICY %I ON catalog.%I AS PERMISSIVE FOR ALL TO wamn_platform USING ((SELECT wamn_authority.current_role()) IS NOT NULL) WITH CHECK ((SELECT wamn_authority.current_role()) IS NOT NULL)',
            relation_name || '_platform', relation_name
        );
    END LOOP;
END
$project_base_grants$;

-- Immutable facts reject both mutation and removal. Heads and connection
-- instances are the deliberately mutable control rows.
DO $immutable_facts$
DECLARE
    relation_name text;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'connection_requirement'
    ] LOOP
        EXECUTE format(
            'CREATE TRIGGER %I BEFORE UPDATE OR DELETE ON catalog.%I FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change()',
            relation_name || '_immutable', relation_name
        );
    END LOOP;
END
$immutable_facts$;

GRANT SELECT ON catalog.package,
    catalog.connection_requirement,
    catalog.workflow
TO wamn_app;

DO $record_history$
DECLARE source record;
BEGIN
    FOR source IN SELECT * FROM (VALUES
        ('platform', 'connection'),
        ('platform', 'connection_generation'),
        ('platform', 'credential'),
        ('platform', 'external_system'),
        ('catalog', 'package'),
        ('catalog', 'connection_requirement'),
        ('catalog', 'workflow')
    ) AS relations(schema_name, relation_name) LOOP
        PERFORM wamn_history.create_history_table(source.schema_name, source.relation_name, false);
        EXECUTE format('CREATE TRIGGER wamn_record_history_log AFTER INSERT OR UPDATE OR DELETE '
                       'ON %I.%I FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change(''unlimited'')',
                       source.schema_name, source.relation_name);
    END LOOP;
END
$record_history$;

REVOKE ALL ON ALL TABLES IN SCHEMA catalog FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA catalog FROM PUBLIC;
