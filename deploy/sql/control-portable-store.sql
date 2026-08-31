-- Portable package, effective-release, authoring, and attestation storage for
-- the T1 control database. Apply after system-schema.sql as wamn_system.
--
-- This is a greenfield declaration. It carries no catalog-document reader,
-- schema-model persistence, upgrade shim, or flow-era release relation.

CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE SCHEMA IF NOT EXISTS catalog AUTHORIZATION wamn_system;
CREATE SCHEMA IF NOT EXISTS wamn_run AUTHORIZATION wamn_system;
REVOKE ALL ON SCHEMA catalog, wamn_run FROM PUBLIC;

CREATE OR REPLACE FUNCTION catalog.reject_immutable_row_change()
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
    tenant_id                    text        NOT NULL CHECK (tenant_id <> ''),
    effective_release_id         int         NOT NULL CHECK (effective_release_id > 0),
    environment                  text        NOT NULL CHECK (environment <> ''),
    verified_publisher_principal text CHECK (
        verified_publisher_principal IS NULL OR verified_publisher_principal <> ''
    ),
    created_at                   timestamptz NOT NULL DEFAULT now(),
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

CREATE TABLE catalog.component_library (
    tenant_id            text        NOT NULL CHECK (tenant_id <> ''),
    package_id           text        NOT NULL CHECK (package_id <> ''),
    package_version      text        NOT NULL CHECK (package_version <> ''),
    component            text        NOT NULL CHECK (component <> ''),
    interface_version    text        NOT NULL CHECK (interface_version <> ''),
    operation            text        NOT NULL CHECK (operation <> ''),
    registered_operation text,
    component_digest     text        NOT NULL CHECK (component_digest ~ '^sha256:[0-9a-f]{64}$'),
    projection_hash      text        NOT NULL CHECK (projection_hash ~ '^sha256:[0-9a-f]{64}$'),
    imports              jsonb       NOT NULL CHECK (jsonb_typeof(imports) = 'array'),
    imports_fingerprint  text        NOT NULL CHECK (imports_fingerprint ~ '^sha256:[0-9a-f]{64}$'),
    effects              jsonb       NOT NULL CHECK (jsonb_typeof(effects) = 'array'),
    input_ports          jsonb       NOT NULL CHECK (jsonb_typeof(input_ports) = 'array'),
    output_ports         jsonb       NOT NULL CHECK (jsonb_typeof(output_ports) = 'array'),
    parameters           jsonb       NOT NULL CHECK (jsonb_typeof(parameters) = 'array'),
    admitted_at          timestamptz NOT NULL DEFAULT now(),
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

CREATE TABLE catalog.authoring_command_audit (
    tenant_id         text        NOT NULL CHECK (tenant_id <> ''),
    audit_id          uuid        NOT NULL DEFAULT gen_random_uuid(),
    command_id        text        NOT NULL CHECK (command_id <> ''),
    command_kind      text        NOT NULL CHECK (command_kind IN ('gate', 'publish')),
    principal_id      text        NOT NULL CHECK (principal_id <> ''),
    principal_kind    text        NOT NULL CHECK (principal_kind IN ('human', 'service')),
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

CREATE TABLE catalog.deployment_attestations (
    tenant_id              text        NOT NULL CHECK (tenant_id <> ''),
    effective_release_id   int         NOT NULL CHECK (effective_release_id > 0),
    org_id                 text        NOT NULL CHECK (org_id <> ''),
    project_id             text        NOT NULL CHECK (project_id <> ''),
    environment            text        NOT NULL CHECK (environment <> ''),
    deployed_manifest_hash text        NOT NULL
        CHECK (deployed_manifest_hash ~ '^sha256:[0-9a-f]{64}$'),
    attested_at            timestamptz NOT NULL,
    CONSTRAINT deployment_attestations_coordinate UNIQUE (
        tenant_id, effective_release_id, org_id, project_id, environment
    ),
    CONSTRAINT deployment_attestations_release_fkey
        FOREIGN KEY (tenant_id, effective_release_id, environment)
        REFERENCES catalog.effective_releases
            (tenant_id, effective_release_id, environment)
);

CREATE OR REPLACE FUNCTION catalog.project_effective_release_identity(
    p_tenant_id text,
    p_effective_release_id int,
    p_environment text
)
RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog.effective_releases (
        tenant_id, effective_release_id, environment
    ) VALUES (
        p_tenant_id, p_effective_release_id, p_environment
    )
    ON CONFLICT (tenant_id, effective_release_id) DO NOTHING;

    IF NOT EXISTS (
        SELECT 1 FROM catalog.effective_releases
         WHERE tenant_id = p_tenant_id
           AND effective_release_id = p_effective_release_id
           AND environment = p_environment
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '23505',
            MESSAGE = 'effective-release-identity-projection-content-conflict';
    END IF;
END
$$;
REVOKE ALL ON FUNCTION catalog.project_effective_release_identity(text, int, text)
    FROM PUBLIC;

CREATE OR REPLACE FUNCTION catalog.register_deployment_attestation(
    p_tenant_id text,
    p_effective_release_id int,
    p_org_id text,
    p_project_id text,
    p_environment text,
    p_deployed_manifest_hash text,
    p_attested_at timestamptz
)
RETURNS timestamptz
LANGUAGE plpgsql
AS $$
DECLARE
    stored_attested_at timestamptz;
BEGIN
    INSERT INTO catalog.deployment_attestations (
        tenant_id, effective_release_id, org_id, project_id, environment,
        deployed_manifest_hash, attested_at
    ) VALUES (
        p_tenant_id, p_effective_release_id, p_org_id, p_project_id,
        p_environment, p_deployed_manifest_hash, p_attested_at
    )
    ON CONFLICT (
        tenant_id, effective_release_id, org_id, project_id, environment
    ) DO NOTHING
    RETURNING attested_at INTO stored_attested_at;

    IF stored_attested_at IS NOT NULL THEN
        RETURN stored_attested_at;
    END IF;

    -- Identical concurrent writers adopt the timestamp of the row that won;
    -- only a different deployed manifest hash conflicts at this coordinate.
    SELECT attested_at INTO stored_attested_at
      FROM catalog.deployment_attestations
     WHERE tenant_id = p_tenant_id
       AND effective_release_id = p_effective_release_id
       AND org_id = p_org_id
       AND project_id = p_project_id
       AND environment = p_environment
       AND deployed_manifest_hash = p_deployed_manifest_hash;

    IF stored_attested_at IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '23505',
            MESSAGE = 'deployment-attestation-content-conflict';
    END IF;
    RETURN stored_attested_at;
END
$$;
REVOKE ALL ON FUNCTION catalog.register_deployment_attestation(
    text, int, text, text, text, text, timestamptz
) FROM PUBLIC;

-- Tenant isolation is structural even for owner-only tables.
DO $tenant_policies$
DECLARE
    relation_name text;
    policy_name text;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'catalog.packages', 'catalog.package_migrations',
        'catalog.effective_releases', 'catalog.effective_release_packages',
        'catalog.effective_release_heads', 'catalog.component_library',
        'catalog.connection_requirements', 'catalog.authoring_command_audit',
        'catalog.deployment_attestations', 'wamn_run.gate_reports'
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
    FOREACH relation_name IN ARRAY ARRAY[
        'catalog.packages', 'catalog.package_migrations',
        'catalog.effective_releases', 'catalog.effective_release_packages',
        'catalog.component_library', 'catalog.connection_requirements',
        'catalog.authoring_command_audit', 'catalog.deployment_attestations',
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
        'catalog.effective_releases', 'catalog.effective_release_heads',
        'catalog.connection_requirements', 'catalog.authoring_command_audit',
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

REVOKE ALL PRIVILEGES ON catalog.effective_releases,
    catalog.effective_release_heads, catalog.connection_requirements
    FROM wamn_control_author;
GRANT SELECT ON catalog.effective_releases,
    catalog.effective_release_heads, catalog.connection_requirements
    TO wamn_control_author;

REVOKE ALL PRIVILEGES ON catalog.authoring_command_audit, wamn_run.gate_reports
    FROM wamn_control_author;
GRANT SELECT, INSERT ON catalog.authoring_command_audit, wamn_run.gate_reports
    TO wamn_control_author;

REVOKE ALL ON ALL TABLES IN SCHEMA catalog, wamn_run FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA catalog FROM PUBLIC;

-- Assert the server inventory produced by this artifact. There is deliberately
-- no checked-in fingerprint or compatibility list to regenerate.
DO $inventory$
DECLARE
    catalog_tables text[];
    run_tables text[];
    unexpected text;
BEGIN
    SELECT array_agg(tablename ORDER BY tablename) INTO catalog_tables
      FROM pg_tables WHERE schemaname = 'catalog';
    IF catalog_tables IS DISTINCT FROM ARRAY[
        'authoring_command_audit', 'component_library',
        'connection_requirements', 'deployment_attestations',
        'effective_release_heads', 'effective_release_packages',
        'effective_releases', 'package_migrations', 'packages'
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
                 ('catalog', 'effective_releases', 'SELECT'),
                 ('catalog', 'effective_release_heads', 'SELECT'),
                 ('catalog', 'connection_requirements', 'SELECT'),
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
$inventory$;
