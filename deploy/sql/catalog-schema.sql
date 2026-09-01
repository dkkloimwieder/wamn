-- Project-environment package, release, wiring, and registration storage.
--
-- Package directories are the authoring truth: raw `wamn.json` bytes identify
-- an immutable package coordinate and ordered migration bytes are recorded in
-- the same transaction that applies them. Effective releases independently
-- compose exact package coordinates. No catalog-document, schema-model, or
-- generated semantic-table registry survives this schema.
--
-- STANDALONE TRANSACTIONAL ARTIFACT. This file is not included by
-- postgres-init.sql; the project-environment provisioner applies it whole.

BEGIN;

CREATE SCHEMA catalog AUTHORIZATION postgres;

DO $platform_group$ BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles
                 WHERE rolname = 'wamn_platform') THEN
    CREATE ROLE wamn_platform NOLOGIN NOSUPERUSER NOCREATEDB
      NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
  END IF;
EXCEPTION WHEN duplicate_object THEN NULL;
END $platform_group$;

GRANT USAGE ON SCHEMA catalog TO wamn_app;
GRANT USAGE ON SCHEMA catalog TO wamn_scenario_author;

-- GENERATED authority-derivation bootstrap. The database name is embedded so
-- tenant_key stays IMMUTABLE and remains legal in expression indexes.
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

CREATE FUNCTION catalog.reject_immutable_row_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = TG_TABLE_SCHEMA || '.' || TG_TABLE_NAME || ' is immutable';
END
$$;
REVOKE ALL ON FUNCTION catalog.reject_immutable_row_change() FROM PUBLIC;

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

CREATE FUNCTION catalog.register_package(
    p_tenant_id text,
    p_package_id text,
    p_package_version text,
    p_manifest_sha256 text,
    p_predecessor_version text
)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    recorded_sha256 text;
    recorded_predecessor_version text;
    current_version text;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'wamn.package.lineage:' || p_tenant_id || ':' || p_package_id, 0));

    SELECT manifest_sha256, predecessor_version
      INTO recorded_sha256, recorded_predecessor_version
      FROM catalog.packages
     WHERE tenant_id = p_tenant_id
       AND package_id = p_package_id
       AND package_version = p_package_version;

    IF FOUND THEN
        IF recorded_sha256 IS DISTINCT FROM p_manifest_sha256 THEN
            RAISE EXCEPTION USING
                ERRCODE = '23505',
                MESSAGE = format(
                    'package-coordinate-content-conflict: coordinate=%s@%s recorded-sha256=%s presented-sha256=%s',
                    p_package_id, p_package_version, recorded_sha256, p_manifest_sha256
                );
        END IF;
        IF recorded_predecessor_version IS DISTINCT FROM p_predecessor_version THEN
            RAISE EXCEPTION USING
                ERRCODE = '23505',
                MESSAGE = format(
                    'package-coordinate-predecessor-conflict: coordinate=%s@%s recorded-predecessor=%s presented-predecessor=%s',
                    p_package_id, p_package_version,
                    coalesce(recorded_predecessor_version, '<none>'),
                    coalesce(p_predecessor_version, '<none>')
                );
        END IF;
        RETURN;
    END IF;

    SELECT package.package_version INTO current_version
      FROM catalog.packages AS package
     WHERE package.tenant_id = p_tenant_id
       AND package.package_id = p_package_id
       AND NOT EXISTS (
           SELECT 1
             FROM catalog.packages AS successor
            WHERE successor.tenant_id = package.tenant_id
              AND successor.package_id = package.package_id
              AND successor.predecessor_version = package.package_version
       );

    IF FOUND AND p_predecessor_version IS DISTINCT FROM current_version THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'predecessor-not-current',
            DETAIL = format(
                'declared=%s current=%s',
                coalesce(p_predecessor_version, '<none>'), current_version
            ),
            HINT = 'declare the current installed package version as predecessor_version';
    END IF;

    INSERT INTO catalog.packages (
        tenant_id, package_id, package_version, predecessor_version, manifest_sha256
    ) VALUES (
        p_tenant_id, p_package_id, p_package_version,
        p_predecessor_version, p_manifest_sha256
    );
END
$$;
REVOKE ALL ON FUNCTION catalog.register_package(text, text, text, text, text) FROM PUBLIC;

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

CREATE TABLE catalog.effective_releases (
    tenant_id                   text        NOT NULL CHECK (tenant_id <> ''),
    effective_release_id        int         NOT NULL CHECK (effective_release_id > 0),
    environment                 text        NOT NULL CHECK (environment <> ''),
    verified_publisher_principal text CHECK (
        verified_publisher_principal IS NULL OR verified_publisher_principal <> ''
    ),
    created_at                  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT effective_releases_pkey
        PRIMARY KEY (tenant_id, effective_release_id),
    CONSTRAINT effective_releases_environment_key
        UNIQUE (tenant_id, effective_release_id, environment)
);

CREATE TABLE catalog.effective_release_packages (
    tenant_id            text NOT NULL CHECK (tenant_id <> ''),
    effective_release_id int  NOT NULL CHECK (effective_release_id > 0),
    package_id           text NOT NULL CHECK (package_id <> ''),
    package_version      text NOT NULL CHECK (package_version <> ''),
    CONSTRAINT effective_release_packages_pkey
        PRIMARY KEY (tenant_id, effective_release_id, package_id),
    CONSTRAINT effective_release_packages_exact_pair_key
        UNIQUE (tenant_id, effective_release_id, package_id, package_version),
    CONSTRAINT effective_release_packages_release_fkey
        FOREIGN KEY (tenant_id, effective_release_id)
        REFERENCES catalog.effective_releases (tenant_id, effective_release_id),
    CONSTRAINT effective_release_packages_package_fkey
        FOREIGN KEY (tenant_id, package_id, package_version)
        REFERENCES catalog.packages (tenant_id, package_id, package_version)
);

-- Immutable release membership is the sole package-coordinate seal. Both the
-- publisher and migration ledger serialize on the package row, so whichever
-- commits first determines whether one last migration precedes the seal or is
-- refused after it. There is no second seal flag or release ledger snapshot.
CREATE FUNCTION catalog.lock_package_coordinate_for_release_membership()
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
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION catalog.lock_package_coordinate_for_release_membership() FROM PUBLIC;

CREATE TRIGGER effective_release_packages_seal_coordinate
    BEFORE INSERT ON catalog.effective_release_packages
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
          FROM catalog.effective_release_packages
         WHERE tenant_id = NEW.tenant_id
           AND package_id = NEW.package_id
           AND package_version = NEW.package_version
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
    tenant_id            text        NOT NULL CHECK (tenant_id <> ''),
    environment          text        NOT NULL CHECK (environment <> ''),
    effective_release_id int         NOT NULL CHECK (effective_release_id > 0),
    updated_at           timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT effective_release_heads_pkey
        PRIMARY KEY (tenant_id, environment),
    CONSTRAINT effective_release_heads_release_fkey
        FOREIGN KEY (tenant_id, effective_release_id, environment)
        REFERENCES catalog.effective_releases
            (tenant_id, effective_release_id, environment)
);

CREATE FUNCTION catalog.publication_boundary(p_stage text)
RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
    IF current_setting('wamn.test.publication_fault', true) = p_stage THEN
        RAISE EXCEPTION USING
            ERRCODE = '40000',
            MESSAGE = 'injected-publication-fault-' || p_stage;
    END IF;
END
$$;
REVOKE ALL ON FUNCTION catalog.publication_boundary(text) FROM PUBLIC;

CREATE TABLE catalog.component_library (
    tenant_id           text        NOT NULL CHECK (tenant_id <> ''),
    package_id          text        NOT NULL CHECK (package_id <> ''),
    package_version     text        NOT NULL CHECK (package_version <> ''),
    component           text        NOT NULL CHECK (component <> ''),
    interface_version   text        NOT NULL CHECK (interface_version <> ''),
    operation           text        NOT NULL CHECK (operation <> ''),
    registered_operation text,
    component_digest    text        NOT NULL CHECK (component_digest ~ '^sha256:[0-9a-f]{64}$'),
    projection_hash     text        NOT NULL CHECK (projection_hash ~ '^sha256:[0-9a-f]{64}$'),
    imports             jsonb       NOT NULL CHECK (jsonb_typeof(imports) = 'array'),
    imports_fingerprint text        NOT NULL CHECK (imports_fingerprint ~ '^sha256:[0-9a-f]{64}$'),
    effects             jsonb       NOT NULL CHECK (jsonb_typeof(effects) = 'array'),
    input_ports         jsonb       NOT NULL CHECK (jsonb_typeof(input_ports) = 'array'),
    output_ports        jsonb       NOT NULL CHECK (jsonb_typeof(output_ports) = 'array'),
    parameters          jsonb       NOT NULL CHECK (jsonb_typeof(parameters) = 'array'),
    admitted_at         timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT component_library_pkey
        PRIMARY KEY (tenant_id, package_id, package_version, component, interface_version),
    CONSTRAINT component_library_package_fkey
        FOREIGN KEY (tenant_id, package_id, package_version)
        REFERENCES catalog.packages (tenant_id, package_id, package_version),
    CONSTRAINT component_library_one_operation_per_digest
        UNIQUE (tenant_id, component_digest),
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
        PRIMARY KEY (tenant_id, component_digest, store_alias),
    CONSTRAINT connection_requirements_component_fkey
        FOREIGN KEY (tenant_id, component_digest)
        REFERENCES catalog.component_library (tenant_id, component_digest)
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
    credential_set_handle text       NOT NULL CHECK (credential_set_handle <> ''),
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
    effective_release_id int         NOT NULL CHECK (effective_release_id > 0),
    component_digest     text        NOT NULL CHECK (component_digest ~ '^sha256:[0-9a-f]{64}$'),
    store_alias          text        NOT NULL CHECK (store_alias <> ''),
    environment          text        NOT NULL CHECK (environment <> ''),
    instance_id          text        NOT NULL CHECK (instance_id <> ''),
    binding_status       text        NOT NULL CHECK (binding_status IN ('active', 'disabled')),
    validation_status    text        NOT NULL CHECK (validation_status IN ('valid', 'invalid')),
    validation_hash      text        NOT NULL CHECK (validation_hash ~ '^sha256:[0-9a-f]{64}$'),
    bound_at             timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT connection_bindings_pkey
        PRIMARY KEY (tenant_id, effective_release_id, component_digest, store_alias),
    CONSTRAINT connection_bindings_release_fkey
        FOREIGN KEY (tenant_id, effective_release_id, environment)
        REFERENCES catalog.effective_releases
            (tenant_id, effective_release_id, environment),
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
        UNIQUE (tenant_id, package_id, wiring_id, wiring_hash)
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

CREATE TABLE catalog.wiring_activation (
    tenant_id                  text        NOT NULL CHECK (tenant_id <> ''),
    package_id                 text        NOT NULL CHECK (package_id <> ''),
    environment                text        NOT NULL CHECK (environment <> ''),
    wiring_id                  text        NOT NULL CHECK (wiring_id <> ''),
    confirmed_definition_hash  text        NOT NULL
        CHECK (confirmed_definition_hash ~ '^sha256:[0-9a-f]{64}$'),
    enabled                    boolean     NOT NULL,
    changed_at                 timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT wiring_activation_pkey
        PRIMARY KEY (tenant_id, package_id, environment, wiring_id)
);

CREATE TABLE catalog.wiring_activation_events (
    event_seq                  bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id                  text        NOT NULL CHECK (tenant_id <> ''),
    package_id                 text        NOT NULL CHECK (package_id <> ''),
    environment                text        NOT NULL CHECK (environment <> ''),
    wiring_id                  text        NOT NULL CHECK (wiring_id <> ''),
    enabled                    boolean     NOT NULL,
    confirmed_definition_hash  text        NOT NULL
        CHECK (confirmed_definition_hash ~ '^sha256:[0-9a-f]{64}$'),
    source_environment         text,
    changed_by                 text        NOT NULL CHECK (changed_by <> ''),
    reason                     text        NOT NULL CHECK (reason <> ''),
    changed_at                 timestamptz NOT NULL DEFAULT now()
);

CREATE FUNCTION catalog.validate_wiring_activation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NOT NEW.enabled THEN
        RETURN NEW;
    END IF;
    IF EXISTS (
        SELECT 1 FROM catalog.wiring_tombstones AS dead
         WHERE dead.tenant_id = NEW.tenant_id
           AND dead.package_id = NEW.package_id
           AND dead.environment = NEW.environment
           AND dead.wiring_id = NEW.wiring_id
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'wiring-activation-tombstoned';
    END IF;
    IF NOT EXISTS (
        SELECT 1
          FROM catalog.wirings AS wiring
          JOIN catalog.effective_release_heads AS head
            ON head.tenant_id = wiring.tenant_id
           AND head.environment = NEW.environment
          JOIN catalog.effective_release_packages AS member
            ON member.tenant_id = head.tenant_id
           AND member.effective_release_id = head.effective_release_id
           AND member.package_id = wiring.package_id
           AND member.package_version = wiring.package_version
         WHERE wiring.tenant_id = NEW.tenant_id
           AND wiring.package_id = NEW.package_id
           AND wiring.wiring_id = NEW.wiring_id
           AND wiring.wiring_hash = NEW.confirmed_definition_hash
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '23503',
            MESSAGE = 'wiring-activation-definition-not-in-effective-release';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER wiring_activation_valid
    BEFORE INSERT OR UPDATE ON catalog.wiring_activation
    FOR EACH ROW EXECUTE FUNCTION catalog.validate_wiring_activation();

CREATE FUNCTION catalog.notify_wiring_activation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM pg_notify(
        'wamn_wiring_activation',
        json_build_object(
            'tenant-id', NEW.tenant_id,
            'package-id', NEW.package_id,
            'environment', NEW.environment,
            'wiring-id', NEW.wiring_id,
            'enabled', NEW.enabled,
            'confirmed-definition-hash', NEW.confirmed_definition_hash
        )::text
    );
    RETURN NEW;
END
$$;
CREATE TRIGGER wiring_activation_doorbell
    AFTER INSERT OR UPDATE ON catalog.wiring_activation
    FOR EACH ROW EXECUTE FUNCTION catalog.notify_wiring_activation();

CREATE TABLE catalog.release_components (
    tenant_id             text NOT NULL CHECK (tenant_id <> ''),
    effective_release_id  int  NOT NULL CHECK (effective_release_id > 0),
    wiring_package_id     text NOT NULL CHECK (wiring_package_id <> ''),
    wiring_package_version text NOT NULL CHECK (wiring_package_version <> ''),
    wiring_id             text NOT NULL CHECK (wiring_id <> ''),
    wiring_version        int  NOT NULL CHECK (wiring_version > 0),
    node_id               text NOT NULL CHECK (node_id <> ''),
    package_id            text NOT NULL CHECK (package_id <> ''),
    package_version       text NOT NULL CHECK (package_version <> ''),
    component_digest      text NOT NULL CHECK (component_digest ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT release_components_pkey
        PRIMARY KEY (tenant_id, effective_release_id, wiring_package_id,
                     wiring_package_version, wiring_id, wiring_version, node_id),
    CONSTRAINT release_components_wiring_membership_fkey
        FOREIGN KEY (tenant_id, effective_release_id, wiring_package_id,
                     wiring_package_version)
        REFERENCES catalog.effective_release_packages
            (tenant_id, effective_release_id, package_id, package_version),
    CONSTRAINT release_components_component_membership_fkey
        FOREIGN KEY (tenant_id, effective_release_id, package_id, package_version)
        REFERENCES catalog.effective_release_packages
            (tenant_id, effective_release_id, package_id, package_version),
    CONSTRAINT release_components_wiring_fkey
        FOREIGN KEY (tenant_id, wiring_package_id, wiring_package_version,
                     wiring_id, wiring_version)
        REFERENCES catalog.wirings
            (tenant_id, package_id, package_version, wiring_id, version),
    CONSTRAINT release_components_component_fkey
        FOREIGN KEY (tenant_id, package_id, package_version, component_digest)
        REFERENCES catalog.component_library
            (tenant_id, package_id, package_version, component_digest)
);

CREATE TABLE catalog.release_manifest_v3_snapshots (
    tenant_id            text  NOT NULL CHECK (tenant_id <> ''),
    effective_release_id int   NOT NULL CHECK (effective_release_id > 0),
    manifest_digest      text  NOT NULL CHECK (manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    canonical_bytes      bytea NOT NULL CHECK (octet_length(canonical_bytes) > 0),
    CONSTRAINT release_manifest_v3_snapshots_pkey
        PRIMARY KEY (tenant_id, effective_release_id),
    CONSTRAINT release_manifest_v3_snapshots_release_fkey
        FOREIGN KEY (tenant_id, effective_release_id)
        REFERENCES catalog.effective_releases (tenant_id, effective_release_id),
    CONSTRAINT release_manifest_v3_snapshots_exact_hash
        CHECK (manifest_digest = 'sha256:' || encode(sha256(canonical_bytes), 'hex'))
);

CREATE FUNCTION catalog.guard_release_component_insert()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM 1 FROM catalog.effective_releases
     WHERE tenant_id = NEW.tenant_id
       AND effective_release_id = NEW.effective_release_id
     FOR UPDATE;
    IF EXISTS (
        SELECT 1 FROM catalog.release_manifest_v3_snapshots
         WHERE tenant_id = NEW.tenant_id
           AND effective_release_id = NEW.effective_release_id
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'effective-release-snapshot-already-sealed';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER release_components_snapshot_seal
    BEFORE INSERT ON catalog.release_components
    FOR EACH ROW EXECUTE FUNCTION catalog.guard_release_component_insert();

CREATE TABLE catalog.event_registrations (
    tenant_id       text  NOT NULL CHECK (tenant_id <> ''),
    package_id      text  NOT NULL CHECK (package_id <> ''),
    registration_id text  NOT NULL CHECK (registration_id <> ''),
    flow_id         text  NOT NULL CHECK (flow_id <> ''),
    entity_id       text  NOT NULL CHECK (entity_id <> ''),
    registration    jsonb NOT NULL,
    CONSTRAINT event_registrations_pkey
        PRIMARY KEY (tenant_id, package_id, registration_id)
);
CREATE INDEX event_registrations_by_entity
    ON catalog.event_registrations (tenant_id, package_id, entity_id);

-- Tenant floors are one mechanism applied to the complete current relation
-- set. The server inventory, rather than checked-in SQL text, proves the result.
DO $tenant_floors$
DECLARE
    relation_name text;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'packages', 'package_migrations', 'effective_releases',
        'effective_release_packages', 'effective_release_heads',
        'component_library', 'connection_requirements', 'connection_instances',
        'connection_generations', 'connection_bindings', 'wirings',
        'wiring_tombstones', 'wiring_activation', 'wiring_activation_events',
        'release_components', 'release_manifest_v3_snapshots',
        'event_registrations'
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

-- Immutable facts reject both mutation and removal. Heads, activation pointers,
-- and connection instances are the deliberately mutable control rows.
DO $immutable_facts$
DECLARE
    relation_name text;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'packages', 'package_migrations', 'effective_releases',
        'effective_release_packages', 'component_library',
        'connection_requirements', 'connection_generations',
        'connection_bindings', 'wirings', 'wiring_tombstones',
        'wiring_activation_events', 'release_components',
        'release_manifest_v3_snapshots'
    ] LOOP
        EXECUTE format(
            'CREATE TRIGGER %I BEFORE UPDATE OR DELETE ON catalog.%I FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change()',
            relation_name || '_immutable', relation_name
        );
    END LOOP;
END
$immutable_facts$;

GRANT SELECT ON catalog.packages,
    catalog.effective_releases,
    catalog.effective_release_packages,
    catalog.effective_release_heads,
    catalog.component_library,
    catalog.connection_requirements,
    catalog.connection_instances,
    catalog.connection_generations,
    catalog.connection_bindings,
    catalog.wirings,
    catalog.wiring_tombstones,
    catalog.wiring_activation,
    catalog.wiring_activation_events,
    catalog.release_components,
    catalog.release_manifest_v3_snapshots,
    catalog.event_registrations
TO wamn_app;

-- Callable-flow admission uses FOR KEY SHARE. PostgreSQL requires UPDATE on at
-- least one column; tenant_id cannot change through the FORCE-RLS floor.
GRANT UPDATE (tenant_id) ON catalog.event_registrations TO wamn_app;

REVOKE ALL ON ALL TABLES IN SCHEMA catalog FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA catalog FROM PUBLIC;

COMMIT;
