-- Package and release facts carry the four NOT NULL record history stamps.
-- Their triggers use wamn_history.stamp_row(), installed by CATALOG_SCHEMA_SQL.
-- Package application binds wamn:apply-package; environment and connection
-- provisioning bind wamn:provisioning. Release connections and bindings also log history.

CREATE TABLE catalog.packages (
    env_key            text        NOT NULL CHECK (env_key <> ''),
    package_id         text        NOT NULL,
    package_version    text        NOT NULL,
    registered_at      timestamptz NOT NULL DEFAULT now(),
    -- Current SQL source, independent of executable release selection.
    -- NULL: wamn dev applied a working tree.
    artifact_digest    text,
    baseline_fingerprint text NOT NULL
        CHECK (baseline_fingerprint ~ '^sha256:[0-9a-f]{64}$'),
    physical_schema_fingerprint text NOT NULL
        CHECK (physical_schema_fingerprint ~ '^sha256:[0-9a-f]{64}$'),
    created_at         timestamptz NOT NULL,
    created_by         uuid        NOT NULL,
    updated_at         timestamptz NOT NULL,
    updated_by         uuid        NOT NULL,
    CONSTRAINT packages_pkey PRIMARY KEY (env_key, package_id),
    CONSTRAINT packages_package_id_check
        CHECK (package_id ~ '^[a-z][a-z0-9]*(_[a-z0-9]+)*$'),
    CONSTRAINT packages_package_version_check CHECK (
        package_version <> ''
        AND package_version = btrim(package_version)
        AND strpos(package_version, '@') = 0
        AND strpos(package_version, '::') = 0
    ),
    CONSTRAINT packages_artifact_digest_check
        CHECK (artifact_digest IS NULL OR artifact_digest ~ '^sha256:[0-9a-f]{64}$')
);

CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON catalog.packages
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- The database instance owns these records. Replacing it starts a new baseline.
CREATE FUNCTION catalog.guard_package_schema()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        NEW.baseline_fingerprint := NEW.physical_schema_fingerprint;
    ELSIF ROW(NEW.env_key, NEW.package_id, NEW.baseline_fingerprint, NEW.registered_at)
          IS DISTINCT FROM
          ROW(OLD.env_key, OLD.package_id, OLD.baseline_fingerprint, OLD.registered_at) THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-baseline-immutable';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER packages_schema_guard
    BEFORE INSERT OR UPDATE ON catalog.packages
    FOR EACH ROW EXECUTE FUNCTION catalog.guard_package_schema();
CREATE TRIGGER packages_delete_immutable
    BEFORE DELETE ON catalog.packages
    FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

CREATE TABLE catalog.package_migrations (
    env_key         text        NOT NULL CHECK (env_key <> ''),
    package_id      text        NOT NULL CHECK (package_id <> ''),
    package_version text        NOT NULL CHECK (package_version <> ''),
    ordinal         int         NOT NULL CHECK (ordinal > 0),
    relative_path   text        NOT NULL CHECK (relative_path ~ '^migrations/[0-9]{4}_[a-z0-9_]+\.sql$'),
    sha256          text        NOT NULL CHECK (sha256 ~ '^sha256:[0-9a-f]{64}$'),
    outcome         text        NOT NULL CHECK (outcome IN ('applied', 'adopted', 'started')),
    applied_at      timestamptz,
    created_at      timestamptz NOT NULL,
    created_by      uuid        NOT NULL,
    updated_at      timestamptz NOT NULL,
    updated_by      uuid        NOT NULL,
    CONSTRAINT package_migrations_pkey
        PRIMARY KEY (env_key, package_id, ordinal),
    CONSTRAINT package_migrations_path_key
        UNIQUE (env_key, package_id, relative_path),
    CONSTRAINT package_migrations_completion_check
        CHECK ((outcome = 'applied') = (applied_at IS NOT NULL))
);

-- A first installation can commit files before its completed package record.
-- Execution identities stay immutable. Explicit adoption can replace asserted bytes.
-- Adoption never supplies an execution timestamp or resolves an uncertain file.
CREATE FUNCTION catalog.guard_migration_outcome()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.outcome = 'adopted' AND OLD.outcome <> 'started'
       AND ROW(NEW.env_key, NEW.package_id, NEW.ordinal)
           IS NOT DISTINCT FROM ROW(OLD.env_key, OLD.package_id, OLD.ordinal) THEN
        RETURN NEW;
    END IF;
    IF ROW(NEW.env_key, NEW.package_id, NEW.package_version, NEW.ordinal,
           NEW.relative_path, NEW.sha256)
       IS DISTINCT FROM
       ROW(OLD.env_key, OLD.package_id, OLD.package_version, OLD.ordinal,
           OLD.relative_path, OLD.sha256)
       OR OLD.outcome <> 'started' OR NEW.outcome <> 'applied' THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-migration-immutable';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER package_migrations_outcome_guard
    BEFORE UPDATE ON catalog.package_migrations
    FOR EACH ROW EXECUTE FUNCTION catalog.guard_migration_outcome();
CREATE TRIGGER package_migrations_delete_immutable
    BEFORE DELETE ON catalog.package_migrations
    FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON catalog.package_migrations
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- Definition identity is independent from the OID-keyed application entity
-- map: shared relations retain their source package while fields and named
-- constraints added by an overlay retain their own package owner.
CREATE TABLE catalog.package_definition_owners (
    env_key                text    NOT NULL CHECK (env_key <> ''),
    schema_name            text    NOT NULL
        CHECK (schema_name ~ '^[a-z][a-z0-9]*(_[a-z0-9]+)*$'),
    relation_name          text    NOT NULL
        CHECK (relation_name ~ '^[a-z][a-z0-9]*(_[a-z0-9]+)*$'),
    definition_type        text    NOT NULL
        CHECK (definition_type IN ('relation', 'field', 'constraint')),
    definition_name        text    NOT NULL
        CHECK (definition_name ~ '^[a-z][a-z0-9]*(_[a-z0-9]+)*$'),
    owner_package_id       text    NOT NULL
        CHECK (owner_package_id ~ '^[a-z][a-z0-9]*(_[a-z0-9]+)*$'),
    client_field_extensible boolean NOT NULL DEFAULT false,
    created_at             timestamptz NOT NULL,
    created_by             uuid        NOT NULL,
    updated_at             timestamptz NOT NULL,
    updated_by             uuid        NOT NULL,
    CONSTRAINT package_definition_owners_pkey PRIMARY KEY (
        env_key, schema_name, relation_name, definition_type, definition_name
    ),
    CONSTRAINT package_definition_owners_relation_shape_check CHECK (
        definition_type <> 'relation' OR definition_name = relation_name
    ),
    CONSTRAINT package_definition_owners_extensibility_check CHECK (
        definition_type = 'relation' OR NOT client_field_extensible
    )
);

CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON catalog.package_definition_owners
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- Cached application releases coexist without selecting executable code. The manifest
-- digest is the release; this row is an immutable cache of the canonical
-- manifest bytes, and the bytes must hash to the digest. env_key is the RLS
-- target of the row, not part of the release identity. Package membership and
-- the component closure are read from the bytes.
CREATE TABLE catalog.releases (
    env_key         text        NOT NULL CHECK (env_key <> ''),
    manifest_digest text        NOT NULL CHECK (manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    canonical_bytes bytea       NOT NULL CHECK (octet_length(canonical_bytes) > 0),
    recorded_at     timestamptz NOT NULL DEFAULT now(),
    created_at      timestamptz NOT NULL,
    created_by      uuid        NOT NULL,
    updated_at      timestamptz NOT NULL,
    updated_by      uuid        NOT NULL,
    CONSTRAINT releases_pkey PRIMARY KEY (env_key, manifest_digest),
    CONSTRAINT releases_exact_hash
        CHECK (manifest_digest = 'sha256:' || encode(sha256(canonical_bytes), 'hex'))
);

CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON catalog.releases
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- Connection projections precede the catalog references to them.
CREATE SCHEMA platform AUTHORIZATION CURRENT_USER;

CREATE TABLE platform.external_system (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE
);

CREATE TABLE platform.integration (
    id uuid PRIMARY KEY,
    integration_type text NOT NULL,
    external_system_id uuid REFERENCES platform.external_system (id),
    name text NOT NULL UNIQUE,
    status text NOT NULL
);

CREATE TABLE platform.connection (
    id uuid PRIMARY KEY,
    integration_id uuid NOT NULL UNIQUE REFERENCES platform.integration (id),
    integration_type text NOT NULL,
    status text NOT NULL
);

GRANT USAGE ON SCHEMA platform TO wamn_app;
GRANT SELECT ON platform.external_system, platform.integration, platform.connection TO wamn_app;

CREATE TABLE catalog.connection_requirement (
    env_key text NOT NULL,
    component_digest text NOT NULL CHECK (component_digest ~ '^sha256:[0-9a-f]{64}$'),
    package_id text NOT NULL,
    name text NOT NULL CHECK (name ~ '^[a-z][a-z0-9-]{0,62}$'),
    interface text NOT NULL,
    mode text NOT NULL CHECK (mode IN ('read', 'write')),
    PRIMARY KEY (env_key, component_digest, package_id, name)
);

CREATE TABLE catalog.release_connection (
    env_key text NOT NULL,
    manifest_digest text NOT NULL,
    connection_id uuid NOT NULL,
    version integer NOT NULL,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (env_key, manifest_digest, connection_id),
    FOREIGN KEY (env_key, manifest_digest) REFERENCES catalog.releases (env_key, manifest_digest),
    FOREIGN KEY (connection_id) REFERENCES platform.connection (id)
);

CREATE TABLE catalog.connection_binding (
    env_key text NOT NULL,
    manifest_digest text NOT NULL,
    component_digest text NOT NULL,
    package_id text NOT NULL,
    name text NOT NULL,
    connection_id uuid NOT NULL,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (env_key, manifest_digest, component_digest, package_id, name),
    FOREIGN KEY (env_key, manifest_digest, connection_id)
        REFERENCES catalog.release_connection (env_key, manifest_digest, connection_id),
    FOREIGN KEY (env_key, component_digest, package_id, name)
        REFERENCES catalog.connection_requirement (env_key, component_digest, package_id, name)
);

DO $connection_history$
DECLARE relation text;
BEGIN
    FOREACH relation IN ARRAY ARRAY['release_connection', 'connection_binding'] LOOP
        EXECUTE format('CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON catalog.%I '
            'FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row(''created_at'', ''created_by'', ''updated_at'', ''updated_by'')', relation);
        PERFORM wamn_history.create_history_table('catalog', relation, false);
        EXECUTE format('CREATE TRIGGER wamn_record_history_log AFTER INSERT OR UPDATE OR DELETE ON catalog.%I '
            'FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change(''unlimited'')', relation);
    END LOOP;
END $connection_history$;

CREATE TABLE catalog.wiring_tombstones (
    env_key     text        NOT NULL CHECK (env_key <> ''),
    package_id  text        NOT NULL CHECK (package_id <> ''),
    environment text        NOT NULL CHECK (environment <> ''),
    wiring_id   text        NOT NULL CHECK (wiring_id <> ''),
    retired_at  timestamptz NOT NULL DEFAULT now(),
    reason      text        NOT NULL CHECK (reason <> ''),
    created_at  timestamptz NOT NULL,
    created_by  uuid        NOT NULL,
    updated_at  timestamptz NOT NULL,
    updated_by  uuid        NOT NULL,
    CONSTRAINT wiring_tombstones_pkey
        PRIMARY KEY (env_key, package_id, environment, wiring_id)
);

CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON catalog.wiring_tombstones
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- One version of one workflow of an environment (docs/plan/workflows.md R8).
-- The control connection writes it after `author_workflow` gates the graph
-- against the serving release. A version row is immutable except `enabled`,
-- and at most one version of a workflow is enabled.
CREATE TABLE catalog.workflows (
    env_key        text        NOT NULL CHECK (env_key <> ''),
    environment    text        NOT NULL CHECK (environment <> ''),
    application    text        NOT NULL CHECK (application <> ''),
    workflow_id    text        NOT NULL CHECK (workflow_id <> ''),
    version        int         NOT NULL CHECK (version > 0),
    graph_json     jsonb       NOT NULL,
    wiring_hash    text        NOT NULL CHECK (wiring_hash ~ '^sha256:[0-9a-f]{64}$'),
    trigger        text        NOT NULL CHECK (trigger IN ('manual', 'schedule', 'event')),
    -- The five-field cron, evaluated in UTC, of a schedule trigger.
    schedule       text,
    source_package text,
    entity         text,
    ops            text[],
    condition      text,
    enable_sequence bigint,
    execution_type text        NOT NULL CHECK (execution_type IN ('role', 'invoker')),
    execution_role text        CHECK (execution_role <> ''),
    enabled        boolean     NOT NULL DEFAULT false,
    created_by     uuid        NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT workflows_pkey
        PRIMARY KEY (env_key, environment, application, workflow_id, version),
    CONSTRAINT workflows_execution_identity_check
        CHECK ((execution_type = 'role') = (execution_role IS NOT NULL)),
    CONSTRAINT workflows_schedule_check
        CHECK ((trigger = 'schedule') = (schedule IS NOT NULL)),
    CONSTRAINT workflows_event_check CHECK (
        (trigger = 'event' AND source_package IS NOT NULL AND source_package <> ''
         AND entity IS NOT NULL AND entity <> '' AND ops IS NOT NULL
         AND cardinality(ops) BETWEEN 1 AND 3
         AND ops <@ ARRAY['insert', 'update', 'delete']::text[]
         AND array_position(ops, NULL) IS NULL
         AND cardinality(array_positions(ops, 'insert')) <= 1
         AND cardinality(array_positions(ops, 'update')) <= 1
         AND cardinality(array_positions(ops, 'delete')) <= 1
         AND (condition IS NULL OR btrim(condition) <> '')
         AND (enable_sequence IS NULL OR enable_sequence >= 0)
         AND (NOT enabled OR enable_sequence IS NOT NULL))
        OR (trigger <> 'event' AND source_package IS NULL AND entity IS NULL
            AND ops IS NULL AND condition IS NULL AND enable_sequence IS NULL))
);
CREATE UNIQUE INDEX workflows_one_enabled
    ON catalog.workflows (env_key, environment, application, workflow_id)
    WHERE enabled;

CREATE FUNCTION catalog.guard_workflow_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF ROW(NEW.env_key, NEW.environment, NEW.application, NEW.workflow_id,
           NEW.version, NEW.graph_json, NEW.wiring_hash, NEW.trigger, NEW.schedule,
           NEW.source_package, NEW.entity, NEW.ops, NEW.condition,
           NEW.execution_type, NEW.execution_role, NEW.created_by, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.env_key, OLD.environment, OLD.application, OLD.workflow_id,
           OLD.version, OLD.graph_json, OLD.wiring_hash, OLD.trigger, OLD.schedule,
           OLD.source_package, OLD.entity, OLD.ops, OLD.condition,
           OLD.execution_type, OLD.execution_role, OLD.created_by, OLD.created_at) THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'workflow-version-immutable';
    END IF;
    IF NEW.enable_sequence IS DISTINCT FROM OLD.enable_sequence AND NOT NEW.enabled THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'workflow-enable-sequence-requires-enable';
    END IF;
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION catalog.guard_workflow_update() FROM PUBLIC;
CREATE TRIGGER workflows_controlled_update
    BEFORE UPDATE ON catalog.workflows
    FOR EACH ROW EXECUTE FUNCTION catalog.guard_workflow_update();
CREATE TRIGGER workflows_delete_immutable
    BEFORE DELETE ON catalog.workflows
    FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

-- Tenant floors are one mechanism applied to the complete current relation
-- set. The server catalog, rather than checked-in SQL text, shows the result.
DO $tenant_floors$
DECLARE
    relation_name text;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'packages', 'package_migrations', 'package_definition_owners',
        'releases',
        'connection_requirement', 'release_connection', 'connection_binding',
        'wiring_tombstones', 'workflows'
    ] LOOP
        EXECUTE format('ALTER TABLE catalog.%I ENABLE ROW LEVEL SECURITY', relation_name);
        EXECUTE format('ALTER TABLE catalog.%I FORCE ROW LEVEL SECURITY', relation_name);
        EXECUTE format(
            'CREATE POLICY %I ON catalog.%I TO wamn_app USING (wamn_authority.tenant_key(env_key) = wamn_authority.current_tenant_key()) WITH CHECK (wamn_authority.tenant_key(env_key) = wamn_authority.current_tenant_key())',
            relation_name || '_tenant', relation_name
        );
        EXECUTE format(
            'CREATE POLICY %I ON catalog.%I AS PERMISSIVE FOR ALL TO wamn_platform USING (true) WITH CHECK (true)',
            relation_name || '_platform', relation_name
        );
        EXECUTE format(
            'CREATE INDEX %I ON catalog.%I ((wamn_authority.tenant_key(env_key)))',
            relation_name || '_tkey', relation_name
        );
    END LOOP;
END
$tenant_floors$;

-- Immutable facts reject both mutation and removal. Heads and connection
-- instances are the deliberately mutable control rows.
DO $immutable_facts$
DECLARE
    relation_name text;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'package_definition_owners',
        'releases',
        'connection_requirement',
        'wiring_tombstones'
    ] LOOP
        EXECUTE format(
            'CREATE TRIGGER %I BEFORE UPDATE OR DELETE ON catalog.%I FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change()',
            relation_name || '_immutable', relation_name
        );
    END LOOP;
END
$immutable_facts$;

GRANT SELECT ON catalog.packages,
    catalog.releases,
    catalog.connection_requirement,
    catalog.release_connection,
    catalog.connection_binding,
    catalog.wiring_tombstones,
    catalog.workflows
TO wamn_app;

REVOKE ALL ON ALL TABLES IN SCHEMA catalog FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA catalog FROM PUBLIC;

COMMIT;
