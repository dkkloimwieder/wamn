CREATE TABLE catalog.packages (
    tenant_id          text        NOT NULL CHECK (tenant_id <> ''),
    package_id         text        NOT NULL,
    package_version    text        NOT NULL,
    predecessor_version text,
    manifest_sha256    text        NOT NULL,
    registered_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT packages_pkey PRIMARY KEY (tenant_id, package_id, package_version),
    CONSTRAINT packages_package_id_check
        CHECK (package_id ~ '^[a-z][a-z0-9]*(_[a-z0-9]+)*$'),
    CONSTRAINT packages_package_version_check CHECK (
        package_version <> ''
        AND package_version = btrim(package_version)
        AND strpos(package_version, '@') = 0
        AND strpos(package_version, '::') = 0
    ),
    CONSTRAINT packages_predecessor_version_check CHECK (
        predecessor_version IS NULL OR (
            predecessor_version <> ''
            AND predecessor_version = btrim(predecessor_version)
            AND strpos(predecessor_version, '@') = 0
            AND strpos(predecessor_version, '::') = 0
        )
    ),
    CONSTRAINT packages_manifest_sha256_check
        CHECK (manifest_sha256 ~ '^sha256:[0-9a-f]{64}$')
);

CREATE UNIQUE INDEX packages_one_successor_per_version
    ON catalog.packages (tenant_id, package_id, predecessor_version)
    WHERE predecessor_version IS NOT NULL;

CREATE TABLE catalog.package_migrations (
    tenant_id       text        NOT NULL CHECK (tenant_id <> ''),
    package_id      text        NOT NULL CHECK (package_id <> ''),
    package_version text        NOT NULL CHECK (package_version <> ''),
    ordinal         int         NOT NULL CHECK (ordinal > 0),
    relative_path   text        NOT NULL CHECK (relative_path ~ '^migrations/[0-9]{4}_[a-z0-9_]+\.sql$'),
    sha256          text        NOT NULL CHECK (sha256 ~ '^sha256:[0-9a-f]{64}$'),
    applied_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT package_migrations_pkey
        PRIMARY KEY (tenant_id, package_id, package_version, ordinal),
    CONSTRAINT package_migrations_path_key
        UNIQUE (tenant_id, package_id, package_version, relative_path),
    CONSTRAINT package_migrations_package_fkey
        FOREIGN KEY (tenant_id, package_id, package_version)
        REFERENCES catalog.packages (tenant_id, package_id, package_version)
);

-- Definition identity is independent from the OID-keyed application entity
-- map: shared relations retain their source package while fields and named
-- constraints added by an overlay retain their own package owner.
CREATE TABLE catalog.package_definition_owners (
    tenant_id              text    NOT NULL CHECK (tenant_id <> ''),
    schema_name            text    NOT NULL
        CHECK (schema_name ~ '^[a-z][a-z0-9]*(_[a-z0-9]+)*$'),
    relation_name          text    NOT NULL
        CHECK (relation_name ~ '^[a-z][a-z0-9]*(_[a-z0-9]+)*$'),
    definition_type        text    NOT NULL
        CHECK (definition_type IN ('relation', 'field', 'constraint', 'synchronization_function', 'synchronization_trigger')),
    definition_name        text    NOT NULL
        CHECK (definition_name ~ '^[a-z][a-z0-9]*(_[a-z0-9]+)*$'),
    owner_package_id       text    NOT NULL
        CHECK (owner_package_id ~ '^[a-z][a-z0-9]*(_[a-z0-9]+)*$'),
    client_field_extensible boolean NOT NULL DEFAULT false,
    CONSTRAINT package_definition_owners_pkey PRIMARY KEY (
        tenant_id, schema_name, relation_name, definition_type, definition_name
    ),
    CONSTRAINT package_definition_owners_relation_shape_check CHECK (
        definition_type <> 'relation' OR definition_name = relation_name
    ),
    CONSTRAINT package_definition_owners_extensibility_check CHECK (
        definition_type = 'relation' OR NOT client_field_extensible
    )
);

CREATE UNIQUE INDEX package_definition_owners_synchronization_function
    ON catalog.package_definition_owners (tenant_id, schema_name, definition_name)
    WHERE definition_type = 'synchronization_function';

-- One installed release (docs/plan/platform-deploy.md R1). The manifest
-- digest is the release; this row is an immutable cache of the canonical
-- manifest bytes, and the bytes must hash to the digest. tenant_id is the RLS
-- scope of the row, not part of the release identity. Package membership and
-- the component closure are read from the bytes.
CREATE TABLE catalog.releases (
    tenant_id       text        NOT NULL CHECK (tenant_id <> ''),
    manifest_digest text        NOT NULL CHECK (manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    canonical_bytes bytea       NOT NULL CHECK (octet_length(canonical_bytes) > 0),
    recorded_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT releases_pkey PRIMARY KEY (tenant_id, manifest_digest),
    CONSTRAINT releases_exact_hash
        CHECK (manifest_digest = 'sha256:' || encode(sha256(canonical_bytes), 'hex'))
);

-- Immutable release membership is the sole package-coordinate seal. Both the
-- release writer and the migration table serialize on the package rows, so
-- whichever commits first determines whether one last migration precedes the
-- seal or is refused after it. There is no second seal flag.
-- Local-target exception: wamn.local_target_comment equal to the full database comment lifts it.
CREATE FUNCTION catalog.lock_package_coordinate_for_release_membership()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM 1
      FROM catalog.packages AS package
      JOIN jsonb_array_elements(
               convert_from(NEW.canonical_bytes, 'UTF8')::jsonb #> '{release,packages}'
           ) AS member(value)
        ON package.package_id = member.value ->> 'package-id'
       AND package.package_version = member.value ->> 'package-version'
     WHERE package.tenant_id = NEW.tenant_id
     FOR UPDATE OF package;
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION catalog.lock_package_coordinate_for_release_membership() FROM PUBLIC;

CREATE TRIGGER releases_seal_coordinate
    BEFORE INSERT ON catalog.releases
    FOR EACH ROW
    EXECUTE FUNCTION catalog.lock_package_coordinate_for_release_membership();

CREATE FUNCTION catalog.reject_package_migration_after_release_membership()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM 1
      FROM catalog.packages
     WHERE tenant_id = NEW.tenant_id
       AND package_id = NEW.package_id
       AND package_version = NEW.package_version
     FOR UPDATE;

    IF EXISTS (
        SELECT 1
          FROM catalog.releases AS release
          CROSS JOIN LATERAL jsonb_array_elements(
               convert_from(release.canonical_bytes, 'UTF8')::jsonb #> '{release,packages}'
          ) AS member(value)
         WHERE release.tenant_id = NEW.tenant_id
           AND member.value ->> 'package-id' = NEW.package_id
           AND member.value ->> 'package-version' = NEW.package_version
    ) AND NOT EXISTS (
        SELECT 1
          FROM pg_catalog.pg_database
         WHERE datname = pg_catalog.current_database()
           AND pg_catalog.shobj_description(oid, 'pg_database')
               = pg_catalog.current_setting('wamn.local_target_comment', true)
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'package-version-sealed',
            DETAIL = format(
                'coordinate=%s@%s belongs to an effective release',
                NEW.package_id, NEW.package_version
            ),
            HINT = 'create and apply a new package version for additional migrations';
    END IF;
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION catalog.reject_package_migration_after_release_membership() FROM PUBLIC;

CREATE TRIGGER package_migrations_release_seal
    BEFORE INSERT ON catalog.package_migrations
    FOR EACH ROW
    EXECUTE FUNCTION catalog.reject_package_migration_after_release_membership();

CREATE TABLE catalog.effective_release_heads (
    tenant_id       text        NOT NULL CHECK (tenant_id <> ''),
    environment     text        NOT NULL CHECK (environment <> ''),
    manifest_digest text        NOT NULL CHECK (manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT effective_release_heads_pkey
        PRIMARY KEY (tenant_id, environment),
    CONSTRAINT effective_release_heads_release_fkey
        FOREIGN KEY (tenant_id, manifest_digest)
        REFERENCES catalog.releases (tenant_id, manifest_digest)
);

-- A component digest belongs to one package for life. Each version of that
-- package may hold it once, and another package may not hold it
-- (docs/plan/kind-to-type.md §4.3.6).
CREATE TABLE catalog.component_digest_owners (
    tenant_id        text NOT NULL CHECK (tenant_id <> ''),
    component_digest text NOT NULL CHECK (component_digest ~ '^sha256:[0-9a-f]{64}$'),
    package_id       text NOT NULL CHECK (package_id <> ''),
    CONSTRAINT component_digest_owners_pkey
        PRIMARY KEY (tenant_id, component_digest),
    CONSTRAINT component_digest_owners_package_key
        UNIQUE (tenant_id, component_digest, package_id)
);

CREATE TABLE catalog.component_library (
    tenant_id           text        NOT NULL CHECK (tenant_id <> ''),
    package_id          text        NOT NULL CHECK (package_id <> ''),
    package_version     text        NOT NULL CHECK (package_version <> ''),
    component           text        NOT NULL CHECK (component <> ''),
    interface_version   text        NOT NULL CHECK (interface_version <> ''),
    operations          jsonb       NOT NULL
        CHECK (jsonb_typeof(operations) = 'object' AND operations <> '{}'::jsonb),
    component_digest    text        NOT NULL CHECK (component_digest ~ '^sha256:[0-9a-f]{64}$'),
    projection_hash     text        NOT NULL CHECK (projection_hash ~ '^sha256:[0-9a-f]{64}$'),
    imports             jsonb       NOT NULL CHECK (jsonb_typeof(imports) = 'array'),
    imports_fingerprint text        NOT NULL CHECK (imports_fingerprint ~ '^sha256:[0-9a-f]{64}$'),
    effects             jsonb       NOT NULL CHECK (jsonb_typeof(effects) = 'array'),
    admitted_at         timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT component_library_pkey
        PRIMARY KEY (tenant_id, package_id, package_version, component, interface_version),
    CONSTRAINT component_library_package_fkey
        FOREIGN KEY (tenant_id, package_id, package_version)
        REFERENCES catalog.packages (tenant_id, package_id, package_version),
    CONSTRAINT component_library_digest_owner_fkey
        FOREIGN KEY (tenant_id, component_digest, package_id)
        REFERENCES catalog.component_digest_owners (tenant_id, component_digest, package_id),
    CONSTRAINT component_library_package_digest_key
        UNIQUE (tenant_id, package_id, package_version, component_digest)
);

CREATE TABLE catalog.connection_requirements (
    tenant_id        text  NOT NULL CHECK (tenant_id <> ''),
    component_digest text  NOT NULL CHECK (component_digest ~ '^sha256:[0-9a-f]{64}$'),
    store_alias      text  NOT NULL CHECK (store_alias <> ''),
    requirement_json jsonb NOT NULL CHECK (jsonb_typeof(requirement_json) = 'object'),
    requirement_hash text  NOT NULL CHECK (requirement_hash ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT connection_requirements_pkey
        PRIMARY KEY (tenant_id, component_digest, store_alias)
);

CREATE TABLE catalog.connection_instances (
    tenant_id        text        NOT NULL CHECK (tenant_id <> ''),
    environment      text        NOT NULL CHECK (environment <> ''),
    instance_id      text        NOT NULL CHECK (instance_id <> ''),
    requirement_type text        NOT NULL CHECK (requirement_type <> ''),
    contract          text        NOT NULL CHECK (contract <> ''),
    lifecycle_status  text        NOT NULL DEFAULT 'enabled'
        CHECK (lifecycle_status IN ('enabled', 'disabled')),
    active_generation bigint,
    revision          bigint      NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT connection_instances_pkey
        PRIMARY KEY (tenant_id, environment, instance_id)
);

CREATE TABLE catalog.connection_generations (
    tenant_id            text        NOT NULL CHECK (tenant_id <> ''),
    environment          text        NOT NULL CHECK (environment <> ''),
    instance_id          text        NOT NULL CHECK (instance_id <> ''),
    generation           bigint      NOT NULL CHECK (generation > 0),
    definition_json      jsonb       NOT NULL,
    definition_hash      text        NOT NULL CHECK (definition_hash ~ '^sha256:[0-9a-f]{64}$'),
    -- NULL for a gcs blobstore, which signs with the host pod's service account.
    credential_set_handle text       CHECK (credential_set_handle <> ''),
    created_at           timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT connection_generations_pkey
        PRIMARY KEY (tenant_id, environment, instance_id, generation),
    CONSTRAINT connection_generations_instance_fkey
        FOREIGN KEY (tenant_id, environment, instance_id)
        REFERENCES catalog.connection_instances (tenant_id, environment, instance_id)
);

ALTER TABLE catalog.connection_instances
    ADD CONSTRAINT connection_instances_active_generation_fkey
    FOREIGN KEY (tenant_id, environment, instance_id, active_generation)
    REFERENCES catalog.connection_generations
        (tenant_id, environment, instance_id, generation)
    DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE catalog.connection_bindings (
    tenant_id            text        NOT NULL CHECK (tenant_id <> ''),
    manifest_digest      text        NOT NULL CHECK (manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    component_digest     text        NOT NULL CHECK (component_digest ~ '^sha256:[0-9a-f]{64}$'),
    store_alias          text        NOT NULL CHECK (store_alias <> ''),
    environment          text        NOT NULL CHECK (environment <> ''),
    instance_id          text        NOT NULL CHECK (instance_id <> ''),
    binding_status       text        NOT NULL CHECK (binding_status IN ('active', 'disabled')),
    validation_status    text        NOT NULL CHECK (validation_status IN ('valid', 'invalid')),
    validation_hash      text        NOT NULL CHECK (validation_hash ~ '^sha256:[0-9a-f]{64}$'),
    bound_at             timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT connection_bindings_pkey
        PRIMARY KEY (tenant_id, manifest_digest, component_digest, store_alias),
    CONSTRAINT connection_bindings_release_fkey
        FOREIGN KEY (tenant_id, manifest_digest)
        REFERENCES catalog.releases (tenant_id, manifest_digest),
    CONSTRAINT connection_bindings_requirement_fkey
        FOREIGN KEY (tenant_id, component_digest, store_alias)
        REFERENCES catalog.connection_requirements
            (tenant_id, component_digest, store_alias),
    CONSTRAINT connection_bindings_instance_fkey
        FOREIGN KEY (tenant_id, environment, instance_id)
        REFERENCES catalog.connection_instances (tenant_id, environment, instance_id)
);

CREATE FUNCTION catalog.guard_connection_instance_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF ROW(NEW.tenant_id, NEW.environment, NEW.instance_id,
           NEW.requirement_type, NEW.contract, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.tenant_id, OLD.environment, OLD.instance_id,
           OLD.requirement_type, OLD.contract, OLD.created_at) THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'connection-instance-identity-is-immutable';
    END IF;
    IF NEW.revision <= OLD.revision THEN
        RAISE EXCEPTION USING ERRCODE = '23514',
            MESSAGE = 'connection-instance-revision-must-advance';
    END IF;
    NEW.updated_at := now();
    RETURN NEW;
END
$$;
CREATE TRIGGER connection_instances_controlled_update
    BEFORE UPDATE ON catalog.connection_instances
    FOR EACH ROW EXECUTE FUNCTION catalog.guard_connection_instance_update();
CREATE TRIGGER connection_instances_delete_immutable
    BEFORE DELETE ON catalog.connection_instances
    FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

CREATE TABLE catalog.wirings (
    tenant_id       text        NOT NULL CHECK (tenant_id <> ''),
    package_id      text        NOT NULL CHECK (package_id <> ''),
    package_version text        NOT NULL CHECK (package_version <> ''),
    wiring_id       text        NOT NULL CHECK (wiring_id <> ''),
    version         int         NOT NULL CHECK (version > 0),
    graph_json      jsonb       NOT NULL,
    wiring_hash     text        NOT NULL CHECK (wiring_hash ~ '^sha256:[0-9a-f]{64}$'),
    created_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT wirings_pkey
        PRIMARY KEY (tenant_id, package_id, package_version, wiring_id, version),
    CONSTRAINT wirings_package_fkey
        FOREIGN KEY (tenant_id, package_id, package_version)
        REFERENCES catalog.packages (tenant_id, package_id, package_version),
    CONSTRAINT wirings_definition_key
        UNIQUE (tenant_id, package_id, package_version, wiring_id, wiring_hash)
);

CREATE TABLE catalog.wiring_tombstones (
    tenant_id   text        NOT NULL CHECK (tenant_id <> ''),
    package_id  text        NOT NULL CHECK (package_id <> ''),
    environment text        NOT NULL CHECK (environment <> ''),
    wiring_id   text        NOT NULL CHECK (wiring_id <> ''),
    retired_at  timestamptz NOT NULL DEFAULT now(),
    reason      text        NOT NULL CHECK (reason <> ''),
    CONSTRAINT wiring_tombstones_pkey
        PRIMARY KEY (tenant_id, package_id, environment, wiring_id)
);

CREATE TABLE catalog.package_upgrade_qualifications (
    tenant_id                   text        NOT NULL CHECK (tenant_id <> ''),
    package_id                  text        NOT NULL CHECK (package_id <> ''),
    candidate_package_version   text        NOT NULL CHECK (candidate_package_version <> ''),
    canonical_bytes             bytea       NOT NULL CHECK (octet_length(canonical_bytes) > 0),
    result_sha256               text        NOT NULL CHECK (result_sha256 ~ '^sha256:[0-9a-f]{64}$'),
    predecessor_manifest_digest text        NOT NULL
        CHECK (predecessor_manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    recorded_at                 timestamptz NOT NULL DEFAULT now(),
    package_artifact_digest     text        NOT NULL
        CHECK (package_artifact_digest ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT package_upgrade_qualifications_pkey
        PRIMARY KEY (tenant_id, package_id, candidate_package_version),
    CONSTRAINT package_upgrade_qualifications_package_fkey
        FOREIGN KEY (tenant_id, package_id, candidate_package_version)
        REFERENCES catalog.packages (tenant_id, package_id, package_version),
    CONSTRAINT package_upgrade_qualifications_predecessor_fkey
        FOREIGN KEY (tenant_id, predecessor_manifest_digest)
        REFERENCES catalog.releases (tenant_id, manifest_digest),
    CONSTRAINT package_upgrade_qualifications_exact_hash
        CHECK (result_sha256 = 'sha256:' || encode(sha256(canonical_bytes), 'hex'))
);

-- Stage progress survives committed batches before the package version advances.
CREATE TABLE catalog.package_upgrade_stages (
    tenant_id            text NOT NULL CHECK (tenant_id <> ''),
    package_id           text NOT NULL CHECK (package_id <> ''),
    package_version      text NOT NULL CHECK (package_version <> ''),
    predecessor_version  text NOT NULL CHECK (predecessor_version <> '' AND predecessor_version <> package_version),
    status               text NOT NULL CHECK (status IN ('in_progress', 'abandoned', 'completed')),
    cursor               jsonb NOT NULL,
    completed_batches    bigint NOT NULL DEFAULT 0 CHECK (completed_batches >= 0),
    package_artifact_digest    text NOT NULL CHECK (package_artifact_digest ~ '^sha256:[0-9a-f]{64}$'),
    predecessor_release_digest text NOT NULL CHECK (predecessor_release_digest ~ '^sha256:[0-9a-f]{64}$'),
    evidence_digest            text NOT NULL CHECK (evidence_digest ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT package_upgrade_stages_pkey PRIMARY KEY (tenant_id, package_id, package_version)
);

CREATE FUNCTION catalog.guard_package_upgrade_stage_change()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-upgrade-stage-cannot-be-deleted';
    END IF;
    IF ROW(NEW.tenant_id, NEW.package_id, NEW.package_version, NEW.predecessor_version,
           NEW.package_artifact_digest, NEW.predecessor_release_digest, NEW.evidence_digest)
       IS DISTINCT FROM
       ROW(OLD.tenant_id, OLD.package_id, OLD.package_version, OLD.predecessor_version,
           OLD.package_artifact_digest, OLD.predecessor_release_digest, OLD.evidence_digest) THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-upgrade-stage-identity-is-immutable';
    END IF;
    IF OLD.status <> 'in_progress' THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-upgrade-stage-is-terminal';
    END IF;
    IF NEW.completed_batches < OLD.completed_batches
       OR NEW.completed_batches > OLD.completed_batches + 1 THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-upgrade-stage-invalid-batch-count';
    END IF;
    IF NEW.completed_batches = OLD.completed_batches AND NEW.cursor IS DISTINCT FROM OLD.cursor THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-upgrade-stage-cursor-requires-batch';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER package_upgrade_stages_guard
    BEFORE UPDATE OR DELETE ON catalog.package_upgrade_stages
    FOR EACH ROW EXECUTE FUNCTION catalog.guard_package_upgrade_stage_change();
REVOKE ALL ON FUNCTION catalog.guard_package_upgrade_stage_change() FROM PUBLIC;
REVOKE ALL ON catalog.package_upgrade_stages FROM PUBLIC, wamn_app;

CREATE TABLE catalog.event_registrations (
    tenant_id       text  NOT NULL CHECK (tenant_id <> ''),
    package_id      text  NOT NULL CHECK (package_id <> ''),
    registration_id text  NOT NULL CHECK (registration_id <> ''),
    entity_id       text  NOT NULL CHECK (entity_id <> ''),
    registration    jsonb NOT NULL,
    CONSTRAINT event_registrations_pkey
        PRIMARY KEY (tenant_id, package_id, registration_id)
);
CREATE INDEX event_registrations_by_entity
    ON catalog.event_registrations (tenant_id, package_id, entity_id);

-- The recorded floor of each installed package (docs/plan/platform-deploy.md
-- R13, R16): the version `env apply` contracted it to at step 9. A floor only
-- advances along `predecessor_version`; `apply` refuses a document that omits
-- it or declares an ancestor of it.
CREATE TABLE catalog.package_floors (
    tenant_id   text        NOT NULL CHECK (tenant_id <> ''),
    package_id  text        NOT NULL,
    version     text        NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT package_floors_pkey PRIMARY KEY (tenant_id, package_id),
    CONSTRAINT package_floors_package_fkey
        FOREIGN KEY (tenant_id, package_id, version)
        REFERENCES catalog.packages (tenant_id, package_id, package_version)
);

-- Tenant floors are one mechanism applied to the complete current relation
-- set. The server catalog, rather than checked-in SQL text, shows the result.
DO $tenant_floors$
DECLARE
    relation_name text;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'packages', 'package_migrations', 'package_definition_owners',
        'releases', 'effective_release_heads',
        'component_digest_owners', 'component_library', 'connection_requirements',
        'connection_instances',
        'connection_generations', 'connection_bindings', 'wirings',
        'wiring_tombstones',
        'package_upgrade_qualifications',
        'event_registrations', 'package_upgrade_stages', 'package_floors'
    ] LOOP
        EXECUTE format('ALTER TABLE catalog.%I ENABLE ROW LEVEL SECURITY', relation_name);
        EXECUTE format('ALTER TABLE catalog.%I FORCE ROW LEVEL SECURITY', relation_name);
        EXECUTE format(
            'CREATE POLICY %I ON catalog.%I TO wamn_app USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()) WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())',
            relation_name || '_tenant', relation_name
        );
        EXECUTE format(
            'CREATE POLICY %I ON catalog.%I AS PERMISSIVE FOR ALL TO wamn_platform USING (true) WITH CHECK (true)',
            relation_name || '_platform', relation_name
        );
        EXECUTE format(
            'CREATE INDEX %I ON catalog.%I ((wamn_authority.tenant_key(tenant_id)))',
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
        'packages', 'package_migrations', 'package_definition_owners',
        'releases', 'component_digest_owners', 'component_library',
        'connection_requirements', 'connection_generations',
        'wirings', 'wiring_tombstones',
        'package_upgrade_qualifications'
    ] LOOP
        EXECUTE format(
            'CREATE TRIGGER %I BEFORE UPDATE OR DELETE ON catalog.%I FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change()',
            relation_name || '_immutable', relation_name
        );
    END LOOP;
END
$immutable_facts$;

-- A binding never changes. `env apply` removes the bindings of a release
-- that left the live set, after the drain (docs/plan/platform-deploy.md R22 (3)).
CREATE TRIGGER connection_bindings_immutable
    BEFORE UPDATE ON catalog.connection_bindings
    FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

GRANT SELECT ON catalog.packages,
    catalog.releases,
    catalog.effective_release_heads,
    catalog.component_digest_owners,
    catalog.component_library,
    catalog.connection_requirements,
    catalog.connection_instances,
    catalog.connection_generations,
    catalog.connection_bindings,
    catalog.wirings,
    catalog.wiring_tombstones,
    catalog.event_registrations
TO wamn_app;

REVOKE ALL ON ALL TABLES IN SCHEMA catalog FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA catalog FROM PUBLIC;

COMMIT;
