-- Metadata catalog storage schema (3.1). The tables that PERSIST the catalog
-- model defined by crates/schema/model — entities, fields, relations, indexes,
-- and constraints — as versioned, tenant-scoped rows.
--
-- This is NOT the per-project *data* schema: the DDL compiler (3.2) reads these
-- rows and emits the actual project tables (`receipts`, `quality_holds`, ...).
-- These tables hold the *definitions* of those tables.
--
-- STANDALONE ARTIFACT: this file is deliberately NOT included by
-- deploy/sql/postgres-init.sql. It is the persistence target the DDL compiler (3.2)
-- and the catalog-API-first POC build (POC-DM1) wire into a project database;
-- shipping it here keeps the 3.1 model and its storage shape reviewable in one
-- place without touching the S2–S6 gate fixtures.
--
-- Security shape mirrors the rest of the platform (s2/s3): the guest-visible
-- application role (`wamn_app`, not owner, no BYPASSRLS) and the distinct
-- host-only `wamn_scenario_author` NOLOGIN role are provisioned before this
-- file. Tenant separation is the `app.tenant` claim injected with SET LOCAL. Every
-- table FORCEs RLS keyed on NULLIF(current_setting('app.tenant', true), ''),
-- which is NULL (=> zero rows) when no claim was injected — Postgres resets a
-- custom GUC to '' (not NULL) after SET LOCAL, and CHECK (tenant_id <> '')
-- forbids a ''-tenant row, so an empty claim matches nothing structurally.
-- (In production the catalog may live
-- in the control plane rather than a project DB; the tenant-scoped RLS shape is
-- the same either way.)

-- TRANSACTIONAL ARTIFACT (wamn-jnms): the file carries its own BEGIN/COMMIT so it
-- applies identically however it is fed to the server. The migration blocks below
-- take ACCESS EXCLUSIVE locks with a bare `LOCK TABLE`, which PostgreSQL refuses
-- outside a transaction block — legal under a multi-statement simple query (what
-- tokio_postgres `batch_execute` sends, the production path in
-- services/ctl/src/publish_catalog.rs), illegal under `psql -f` autocommit and
-- under any other per-statement applier. Owning the transaction here removes that
-- dependence: `BEGIN` inside the implicit transaction of a simple query merely
-- promotes it to an explicit one, so the production path still applies the file
-- atomically, exactly as before. Nothing in this file is a statement PostgreSQL
-- forbids inside a transaction block, so the whole-file span is safe.
BEGIN;

CREATE SCHEMA catalog AUTHORIZATION postgres;
GRANT USAGE ON SCHEMA catalog TO wamn_app;
GRANT USAGE ON SCHEMA catalog TO wamn_scenario_author;

-- ---------------------------------------------------------------------------
-- Catalog header: one row per (catalog_id, version) — the unit versioned and
-- promoted between environments (3.4, crates/schema/control/src/lifecycle). `schema_version` is
-- the catalog-MODEL format version (crates/schema/model SCHEMA_VERSION),
-- distinct from `version`.
--
-- Lifecycle (3.4): `state` carries the draft -> staged -> applied -> superseded
-- lifecycle (generalizing the earlier `active` boolean); its values are exactly
-- crates/schema/control/src/lifecycle State::as_sql, tied to the crate by a test. `environment`
-- (dev/canary/prod = the closed wamn_registry::Env set, tied to the crate by a
-- test; = a project-env database in the 2.2/2.3 per-project-DB model) makes the
-- deployment target first-class. Version numbers are GLOBALLY UNIQUE per catalog
-- (promotion mints a fresh version in the target environment), so `environment`
-- is an attribute of each version, not part of its identity. `base_version` is
-- the applied version a draft/staged one was branched from — the stale-base
-- (rebase) guard: a staged candidate may be applied only while its base is still
-- the environment's current applied version.
--
-- The single-applied invariant is a partial UNIQUE INDEX: at most one `applied`
-- version per (catalog, environment).
--
-- `document` is the full catalog JSON (crates/schema/model Catalog) for this
-- version — written by the migration engine (2.5, crates/schema/control) as the
-- diff source: the next migration reads the applied version's `document` to diff
-- a target against it. Nullable (populated for versions the engine applies).
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.catalogs (
    tenant_id      text NOT NULL CHECK (tenant_id <> ''),
    catalog_id     text NOT NULL,
    version        int  NOT NULL,
    environment    text NOT NULL DEFAULT 'dev',
    schema_version text NOT NULL,
    name           text,
    state          text NOT NULL DEFAULT 'draft',
    base_version   int,
    document       jsonb,
    PRIMARY KEY (tenant_id, catalog_id, version),
    CONSTRAINT catalogs_state_check
        CHECK (state IN ('draft', 'staged', 'applied', 'superseded'))
    -- `environment` is a validated slug (D18, wamn-8df.3) — no closed CHECK; the
    -- default set (dev/prod) is data in the system registry's env_policies. A
    -- tenant catalog DB cannot FK the system registry, so env is a free label here.
);
ALTER TABLE catalog.catalogs ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.catalogs FORCE ROW LEVEL SECURITY;
CREATE POLICY catalogs_tenant ON catalog.catalogs
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
-- wamn-0h0g.12.20: every production writer is the superuser publish/migrate
-- shell, so the guest-reachable app LOGIN reads this relation and never writes it.
GRANT SELECT ON catalog.catalogs TO wamn_app;

-- Single-applied invariant: exactly one live version per (catalog, environment).
CREATE UNIQUE INDEX catalogs_one_applied_per_env
    ON catalog.catalogs (tenant_id, catalog_id, environment)
    WHERE state = 'applied';

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

-- THE RELEASE IDENTITY ROW: one row per (tenant, catalog, version). It carries
-- no manifest bytes and never did after wamn-0h0g.15.159 dropped the sealed
-- member snapshot, which is why wamn-0h0g.15.162 renamed it from
-- `release_manifests` — the serving manifest is assembled elsewhere, and nobody
-- reading this name should go hunting here for it.
--
-- It has exactly three jobs.
--
-- 1. IDENTITY / IDEMPOTENCY ANCHOR. Its insert IS the "this release exists"
--    event, so publish step A's ON CONFLICT verify-identical makes
--    retry-never-remints structural rather than checked. A release whose
--    existence were only the presence of member rows would have no single row
--    to conflict on and idempotency would degrade to counting.
-- 2. PUBLICATION PROVENANCE. `verified_publisher_principal` is minted inside the
--    publish transaction by the verb that verified the PAT. It is a fact about
--    the PUBLICATION EVENT, not about any flow, so no member or evidence row can
--    carry it.
-- 3. MEMBERSHIP FK ROOT. wamn_run.runs references it, which is what makes
--    "run pinned to a release that does not exist" unrepresentable.
CREATE TABLE catalog.releases (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    catalog_version int  NOT NULL,
    -- Never manufacture a publisher identity from the database/service
    -- login.
    verified_publisher_principal text
        CHECK (verified_publisher_principal IS NULL OR verified_publisher_principal <> ''),
    PRIMARY KEY (tenant_id, catalog_id, catalog_version),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.catalogs (tenant_id, catalog_id, version)
);
ALTER TABLE catalog.releases ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.releases FORCE ROW LEVEL SECURITY;
CREATE POLICY releases_tenant ON catalog.releases
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.releases TO wamn_app;
GRANT SELECT ON catalog.releases TO wamn_scenario_author;
CREATE TRIGGER releases_immutable
BEFORE UPDATE ON catalog.releases
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
CREATE TRIGGER releases_delete_immutable
BEFORE DELETE ON catalog.releases
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

-- BEGIN DISPOSITION PROVENANCE STORAGE MIGRATION (wamn-4u7p.42)
-- Additive upgrade for catalogs provisioned before verified publication
-- provenance existed. Existing rows deliberately remain NULL/unverified.
ALTER TABLE catalog.releases
    ADD COLUMN IF NOT EXISTS verified_publisher_principal text;
ALTER TABLE catalog.releases
    DROP CONSTRAINT IF EXISTS releases_verified_publisher_principal_check;
ALTER TABLE catalog.releases
    ADD CONSTRAINT releases_verified_publisher_principal_check
    CHECK (verified_publisher_principal IS NULL OR verified_publisher_principal <> '');
-- END DISPOSITION PROVENANCE STORAGE MIGRATION (wamn-4u7p.42)

-- Insert-or-verify-identical on the release identity row. This is where
-- `catalog-release-content-conflict` is defined as a database error, and
-- wamn-0h0g.15.159 NARROWED what it means here without renaming it: with the
-- sealed member snapshot gone, this raise no longer defends aggregate bytes, it
-- defends the identity row's own remaining content. A plain DO NOTHING would
-- bless header drift, so the conflict arm re-verifies every column the INSERT
-- supplies -- the first three are the conflict target, leaving
-- `verified_publisher_principal`, which this function always supplies as NULL
-- because publication provenance is minted by the publishing path, not here.
CREATE FUNCTION catalog.register_release_manifest(
    p_tenant_id text,
    p_catalog_id text,
    p_catalog_version int
)
RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog.releases (
        tenant_id, catalog_id, catalog_version
    )
    VALUES (p_tenant_id, p_catalog_id, p_catalog_version)
    ON CONFLICT (tenant_id, catalog_id, catalog_version) DO NOTHING;

    IF NOT EXISTS (
        SELECT 1
        FROM catalog.releases
        WHERE tenant_id = p_tenant_id
          AND catalog_id = p_catalog_id
          AND catalog_version = p_catalog_version
          AND verified_publisher_principal IS NULL
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = '23505',
            MESSAGE = 'catalog-release-content-conflict';
    END IF;
END
$$;
REVOKE ALL ON FUNCTION catalog.register_release_manifest(
    text, text, int
) FROM PUBLIC;

-- A superuser-only fault boundary used by the release gate to prove that the
-- production transaction rolls every definition write back. Normal sessions
-- have no `wamn.test.publication_fault` setting, so this is a no-op.
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

-- The stable row locked by every publication into an environment. The row
-- identity never changes; only its applied release pointer advances.
CREATE TABLE catalog.catalog_heads (
    tenant_id              text NOT NULL CHECK (tenant_id <> ''),
    catalog_id             text NOT NULL,
    environment            text NOT NULL,
    applied_catalog_version int NOT NULL,
    updated_at              timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, catalog_id, environment),
    FOREIGN KEY (tenant_id, catalog_id, applied_catalog_version)
        REFERENCES catalog.catalogs (tenant_id, catalog_id, version)
);
ALTER TABLE catalog.catalog_heads ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.catalog_heads FORCE ROW LEVEL SECURITY;
CREATE POLICY catalog_heads_tenant ON catalog.catalog_heads
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.catalog_heads TO wamn_app;
GRANT SELECT ON catalog.catalog_heads TO wamn_scenario_author;

-- BEGIN CONNECTION STORAGE MIGRATION (wamn-ko5r.6)
-- Portable requirements retain their truthful minting grain. Legacy flow rows
-- keep (artifact_hash, requirement_name); component rows use
-- (component_digest, store_alias). No migration reinterprets an artifact hash
-- as a component digest. Every other record in this block is environment-owned
-- and therefore absent from artifact or component bytes.
CREATE TABLE catalog.connection_requirements (
    tenant_id        text NOT NULL CHECK (tenant_id <> ''),
    artifact_hash    text CHECK (artifact_hash <> ''),
    requirement_name text CHECK (requirement_name <> ''),
    component_digest text CHECK (component_digest <> ''),
    store_alias      text CHECK (store_alias <> ''),
    requirement_json jsonb NOT NULL CHECK (jsonb_typeof(requirement_json) = 'object'),
    requirement_hash text NOT NULL CHECK (requirement_hash <> ''),
    created_at        timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT connection_requirements_complete_grain CHECK (
        (artifact_hash IS NOT NULL AND requirement_name IS NOT NULL
         AND component_digest IS NULL AND store_alias IS NULL)
        OR
        (artifact_hash IS NULL AND requirement_name IS NULL
         AND component_digest IS NOT NULL AND store_alias IS NOT NULL)
    )
);
CREATE UNIQUE INDEX connection_requirements_legacy_key
    ON catalog.connection_requirements (tenant_id, artifact_hash, requirement_name)
    WHERE artifact_hash IS NOT NULL;
CREATE UNIQUE INDEX connection_requirements_component_key
    ON catalog.connection_requirements (tenant_id, component_digest, store_alias)
    WHERE component_digest IS NOT NULL;
ALTER TABLE catalog.connection_requirements ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.connection_requirements FORCE ROW LEVEL SECURITY;
CREATE POLICY connection_requirements_tenant ON catalog.connection_requirements
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.connection_requirements TO wamn_app;
GRANT SELECT ON catalog.connection_requirements TO wamn_scenario_author;
CREATE TRIGGER connection_requirements_immutable
BEFORE UPDATE OR DELETE ON catalog.connection_requirements
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

CREATE TABLE catalog.connection_instances (
    tenant_id         text NOT NULL CHECK (tenant_id <> ''),
    environment       text NOT NULL CHECK (environment <> ''),
    instance_id       text NOT NULL CHECK (instance_id <> ''),
    requirement_type  text NOT NULL CHECK (requirement_type <> ''),
    contract           text NOT NULL CHECK (contract <> ''),
    lifecycle_status   text NOT NULL DEFAULT 'enabled'
        CHECK (lifecycle_status IN ('enabled', 'disabled')),
    active_generation  bigint CHECK (active_generation > 0),
    revision           bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, environment, instance_id),
    CONSTRAINT connection_instances_disabled_pointer CHECK (
        lifecycle_status = 'enabled' OR active_generation IS NULL
    )
);
ALTER TABLE catalog.connection_instances ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.connection_instances FORCE ROW LEVEL SECURITY;
CREATE POLICY connection_instances_tenant ON catalog.connection_instances
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.connection_instances TO wamn_app;
GRANT SELECT ON catalog.connection_instances TO wamn_scenario_author;

CREATE FUNCTION catalog.guard_connection_instance_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF (NEW.tenant_id, NEW.environment, NEW.instance_id,
        NEW.requirement_type, NEW.contract, NEW.created_at)
       IS DISTINCT FROM
       (OLD.tenant_id, OLD.environment, OLD.instance_id,
        OLD.requirement_type, OLD.contract, OLD.created_at)
       OR NEW.revision <> OLD.revision + 1
       OR NEW.updated_at <= OLD.updated_at THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'connection-instance-uncontrolled-update';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER connection_instance_controlled_update
BEFORE UPDATE ON catalog.connection_instances
FOR EACH ROW EXECUTE FUNCTION catalog.guard_connection_instance_update();
CREATE TRIGGER connection_instances_delete_immutable
BEFORE DELETE ON catalog.connection_instances
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

CREATE TABLE catalog.connection_generations (
    tenant_id             text NOT NULL CHECK (tenant_id <> ''),
    environment           text NOT NULL CHECK (environment <> ''),
    instance_id           text NOT NULL CHECK (instance_id <> ''),
    generation            bigint NOT NULL CHECK (generation > 0),
    definition_json       jsonb NOT NULL CHECK (jsonb_typeof(definition_json) = 'object'),
    definition_hash       text NOT NULL CHECK (definition_hash <> ''),
    credential_set_handle text NOT NULL CHECK (credential_set_handle <> ''),
    created_at             timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, environment, instance_id, generation),
    UNIQUE (tenant_id, environment, instance_id, definition_hash),
    FOREIGN KEY (tenant_id, environment, instance_id)
        REFERENCES catalog.connection_instances (tenant_id, environment, instance_id)
);
ALTER TABLE catalog.connection_generations ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.connection_generations FORCE ROW LEVEL SECURITY;
CREATE POLICY connection_generations_tenant ON catalog.connection_generations
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.connection_generations TO wamn_app;
GRANT SELECT ON catalog.connection_generations TO wamn_scenario_author;
CREATE TRIGGER connection_generations_update_immutable
BEFORE UPDATE ON catalog.connection_generations
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

ALTER TABLE catalog.connection_instances
    ADD CONSTRAINT connection_instances_active_generation_fk
    FOREIGN KEY (tenant_id, environment, instance_id, active_generation)
    REFERENCES catalog.connection_generations
        (tenant_id, environment, instance_id, generation)
    DEFERRABLE INITIALLY IMMEDIATE;

CREATE TABLE catalog.connection_bindings (
    tenant_id        text NOT NULL CHECK (tenant_id <> ''),
    catalog_id       text NOT NULL CHECK (catalog_id <> ''),
    catalog_version  int NOT NULL CHECK (catalog_version > 0),
    artifact_hash    text CHECK (artifact_hash <> ''),
    requirement_name text CHECK (requirement_name <> ''),
    component_digest text CHECK (component_digest <> ''),
    store_alias      text CHECK (store_alias <> ''),
    environment      text NOT NULL CHECK (environment <> ''),
    instance_id      text NOT NULL CHECK (instance_id <> ''),
    binding_status   text NOT NULL DEFAULT 'active'
        CHECK (binding_status IN ('active', 'disabled')),
    validation_status text NOT NULL
        CHECK (validation_status IN ('valid', 'invalid')),
    validation_hash  text NOT NULL CHECK (validation_hash <> ''),
    created_at       timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT connection_bindings_complete_grain CHECK (
        (artifact_hash IS NOT NULL AND requirement_name IS NOT NULL
         AND component_digest IS NULL AND store_alias IS NULL)
        OR
        (artifact_hash IS NULL AND requirement_name IS NULL
         AND component_digest IS NOT NULL AND store_alias IS NOT NULL)
    ),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.releases (tenant_id, catalog_id, catalog_version),
    FOREIGN KEY (tenant_id, environment, instance_id)
        REFERENCES catalog.connection_instances (tenant_id, environment, instance_id)
);
CREATE UNIQUE INDEX connection_bindings_legacy_key
    ON catalog.connection_bindings (
        tenant_id, catalog_id, catalog_version, artifact_hash, requirement_name
    ) WHERE artifact_hash IS NOT NULL;
CREATE UNIQUE INDEX connection_bindings_component_key
    ON catalog.connection_bindings (
        tenant_id, catalog_id, catalog_version, component_digest, store_alias
    ) WHERE component_digest IS NOT NULL;
ALTER TABLE catalog.connection_bindings ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.connection_bindings FORCE ROW LEVEL SECURITY;
CREATE POLICY connection_bindings_tenant ON catalog.connection_bindings
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.connection_bindings TO wamn_app;
GRANT SELECT ON catalog.connection_bindings TO wamn_scenario_author;
CREATE TRIGGER connection_bindings_immutable
BEFORE UPDATE OR DELETE ON catalog.connection_bindings
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

CREATE TABLE catalog.connection_generation_retention (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    environment     text NOT NULL CHECK (environment <> ''),
    instance_id     text NOT NULL CHECK (instance_id <> ''),
    generation      bigint NOT NULL CHECK (generation > 0),
    reference_kind  text NOT NULL
        CHECK (reference_kind IN ('active-attempt', 'deployed-release')),
    reference_id    text NOT NULL CHECK (reference_id <> ''),
    retained_until  timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (
        tenant_id, environment, instance_id, generation, reference_kind, reference_id
    ),
    FOREIGN KEY (tenant_id, environment, instance_id, generation)
        REFERENCES catalog.connection_generations
            (tenant_id, environment, instance_id, generation)
);
ALTER TABLE catalog.connection_generation_retention ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.connection_generation_retention FORCE ROW LEVEL SECURITY;
CREATE POLICY connection_generation_retention_tenant
    ON catalog.connection_generation_retention
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.connection_generation_retention TO wamn_app;

CREATE FUNCTION catalog.guard_connection_retention_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF (NEW.tenant_id, NEW.environment, NEW.instance_id, NEW.generation,
        NEW.reference_kind, NEW.reference_id, NEW.created_at)
       IS DISTINCT FROM
       (OLD.tenant_id, OLD.environment, OLD.instance_id, OLD.generation,
        OLD.reference_kind, OLD.reference_id, OLD.created_at)
       OR (OLD.retained_until IS NULL AND NEW.retained_until IS NOT NULL)
       OR (OLD.retained_until IS NOT NULL
           AND NEW.retained_until IS NOT NULL
           AND NEW.retained_until < OLD.retained_until) THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'connection-generation-retention-cannot-shorten';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER connection_generation_retention_controlled_update
BEFORE UPDATE ON catalog.connection_generation_retention
FOR EACH ROW EXECUTE FUNCTION catalog.guard_connection_retention_update();

CREATE FUNCTION catalog.reject_referenced_connection_generation_delete()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM catalog.connection_generation_retention retention
        WHERE retention.tenant_id = OLD.tenant_id
          AND retention.environment = OLD.environment
          AND retention.instance_id = OLD.instance_id
          AND retention.generation = OLD.generation
          AND (retention.retained_until IS NULL OR retention.retained_until > now())
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'connection-generation-retained';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER connection_generations_delete_retained
BEFORE DELETE ON catalog.connection_generations
FOR EACH ROW EXECUTE FUNCTION catalog.reject_referenced_connection_generation_delete();
-- END CONNECTION STORAGE MIGRATION (wamn-ko5r.6)

-- BEGIN CONNECTION COMPONENT GRAIN MIGRATION (wamn-0h0g.21.4)
-- Existing artifact rows remain legacy facts. The migration adds nullable
-- component coordinates and deliberately performs no backfill: artifact hashes
-- name flow artifacts, not component bytes.
LOCK TABLE catalog.connection_requirements, catalog.connection_bindings
    IN ACCESS EXCLUSIVE MODE;
DO $drop_legacy_connection_requirement_fk$
DECLARE
    constraint_name text;
BEGIN
    FOR constraint_name IN
        SELECT con.conname
        FROM pg_constraint con
        WHERE con.conrelid = 'catalog.connection_bindings'::regclass
          AND con.confrelid = 'catalog.connection_requirements'::regclass
          AND con.contype = 'f'
    LOOP
        EXECUTE format(
            'ALTER TABLE catalog.connection_bindings DROP CONSTRAINT %I',
            constraint_name
        );
    END LOOP;
END
$drop_legacy_connection_requirement_fk$;

ALTER TABLE catalog.connection_bindings
    DROP CONSTRAINT IF EXISTS connection_bindings_pkey,
    ADD COLUMN IF NOT EXISTS component_digest text,
    ADD COLUMN IF NOT EXISTS store_alias text,
    ALTER COLUMN artifact_hash DROP NOT NULL,
    ALTER COLUMN requirement_name DROP NOT NULL,
    DROP CONSTRAINT IF EXISTS connection_bindings_complete_grain,
    ADD CONSTRAINT connection_bindings_complete_grain CHECK (
        (artifact_hash IS NOT NULL AND requirement_name IS NOT NULL
         AND component_digest IS NULL AND store_alias IS NULL)
        OR
        (artifact_hash IS NULL AND requirement_name IS NULL
         AND component_digest IS NOT NULL AND store_alias IS NOT NULL)
    ),
    DROP CONSTRAINT IF EXISTS connection_bindings_component_digest_check,
    ADD CONSTRAINT connection_bindings_component_digest_check
        CHECK (component_digest IS NULL OR component_digest <> ''),
    DROP CONSTRAINT IF EXISTS connection_bindings_store_alias_check,
    ADD CONSTRAINT connection_bindings_store_alias_check
        CHECK (store_alias IS NULL OR store_alias <> '');

ALTER TABLE catalog.connection_requirements
    DROP CONSTRAINT IF EXISTS connection_requirements_pkey,
    ADD COLUMN IF NOT EXISTS component_digest text,
    ADD COLUMN IF NOT EXISTS store_alias text,
    ALTER COLUMN artifact_hash DROP NOT NULL,
    ALTER COLUMN requirement_name DROP NOT NULL,
    DROP CONSTRAINT IF EXISTS connection_requirements_complete_grain,
    ADD CONSTRAINT connection_requirements_complete_grain CHECK (
        (artifact_hash IS NOT NULL AND requirement_name IS NOT NULL
         AND component_digest IS NULL AND store_alias IS NULL)
        OR
        (artifact_hash IS NULL AND requirement_name IS NULL
         AND component_digest IS NOT NULL AND store_alias IS NOT NULL)
    ),
    DROP CONSTRAINT IF EXISTS connection_requirements_component_digest_check,
    ADD CONSTRAINT connection_requirements_component_digest_check
        CHECK (component_digest IS NULL OR component_digest <> ''),
    DROP CONSTRAINT IF EXISTS connection_requirements_store_alias_check,
    ADD CONSTRAINT connection_requirements_store_alias_check
        CHECK (store_alias IS NULL OR store_alias <> '');

CREATE UNIQUE INDEX IF NOT EXISTS connection_requirements_legacy_key
    ON catalog.connection_requirements (tenant_id, artifact_hash, requirement_name)
    WHERE artifact_hash IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS connection_requirements_component_key
    ON catalog.connection_requirements (tenant_id, component_digest, store_alias)
    WHERE component_digest IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS connection_bindings_legacy_key
    ON catalog.connection_bindings (
        tenant_id, catalog_id, catalog_version, artifact_hash, requirement_name
    ) WHERE artifact_hash IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS connection_bindings_component_key
    ON catalog.connection_bindings (
        tenant_id, catalog_id, catalog_version, component_digest, store_alias
    ) WHERE component_digest IS NOT NULL;

DROP TRIGGER IF EXISTS connection_requirements_require_artifact
    ON catalog.connection_requirements;
DROP FUNCTION IF EXISTS catalog.require_connection_artifact();

CREATE OR REPLACE FUNCTION catalog.require_connection_binding_requirement()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    -- Let the table CHECK own malformed partial or mixed coordinates.
    IF NEW.artifact_hash IS NOT NULL AND NEW.requirement_name IS NOT NULL
       AND NEW.component_digest IS NULL AND NEW.store_alias IS NULL THEN
        IF NOT EXISTS (
            SELECT 1 FROM catalog.connection_requirements requirement
            WHERE requirement.tenant_id = NEW.tenant_id
              AND requirement.artifact_hash = NEW.artifact_hash
              AND requirement.requirement_name = NEW.requirement_name
              AND requirement.component_digest IS NULL
              AND requirement.store_alias IS NULL
        ) THEN
            RAISE EXCEPTION USING
                ERRCODE = '23503',
                MESSAGE = 'connection-binding-requirement-missing';
        END IF;
    ELSIF NEW.artifact_hash IS NULL AND NEW.requirement_name IS NULL
          AND NEW.component_digest IS NOT NULL AND NEW.store_alias IS NOT NULL THEN
        IF NOT EXISTS (
            SELECT 1 FROM catalog.connection_requirements requirement
            WHERE requirement.tenant_id = NEW.tenant_id
              AND requirement.artifact_hash IS NULL
              AND requirement.requirement_name IS NULL
              AND requirement.component_digest = NEW.component_digest
              AND requirement.store_alias = NEW.store_alias
        ) THEN
            RAISE EXCEPTION USING
                ERRCODE = '23503',
                MESSAGE = 'connection-binding-requirement-missing';
        END IF;
    END IF;
    RETURN NEW;
END
$$;
DROP TRIGGER IF EXISTS connection_bindings_require_requirement
    ON catalog.connection_bindings;
CREATE TRIGGER connection_bindings_require_requirement
BEFORE INSERT ON catalog.connection_bindings
FOR EACH ROW EXECUTE FUNCTION catalog.require_connection_binding_requirement();

CREATE OR REPLACE FUNCTION catalog.require_binding_release_environment()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM catalog.catalogs release
        WHERE release.tenant_id = NEW.tenant_id
          AND release.catalog_id = NEW.catalog_id
          AND release.version = NEW.catalog_version
          AND release.environment = NEW.environment
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = '23514',
            MESSAGE = 'connection-binding-environment-mismatch';
    END IF;
    RETURN NEW;
END
$$;
DROP TRIGGER IF EXISTS connection_bindings_match_release_environment
    ON catalog.connection_bindings;
CREATE TRIGGER connection_bindings_match_release_environment
BEFORE INSERT ON catalog.connection_bindings
FOR EACH ROW EXECUTE FUNCTION catalog.require_binding_release_environment();
-- END CONNECTION COMPONENT GRAIN MIGRATION (wamn-0h0g.21.4)


-- ---------------------------------------------------------------------------
-- Migration history (2.5, crates/schema/control). One IMMUTABLE row per applied
-- migration — the versioned, forward-only apply journal the migration engine
-- writes inside the SAME transaction as the DDL + the lifecycle advance. A row
-- records the (from -> to) version step, whether it was destructive, the
-- operation count, and a checksum of the applied DDL script (integrity/audit).
-- Destructive authorization evidence lives only in the operations database.
-- `from_version` is NULL for the
-- first materialization of a catalog. Forward-only: the PK forbids recording the
-- same (catalog, environment, to_version) twice. The journal row is appended by
-- the superuser migrate/publish shell inside that same transaction, so
-- wamn-0h0g.12.21 leaves wamn_app SELECT only.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.schema_migrations (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    -- `environment` is a validated slug (D18, wamn-8df.3) — no closed CHECK.
    environment     text NOT NULL,
    from_version    int,
    to_version      int  NOT NULL,
    statement_count int  NOT NULL,
    destructive     boolean NOT NULL DEFAULT false,
    checksum        text NOT NULL,
    applied_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, catalog_id, environment, to_version)
);
ALTER TABLE catalog.schema_migrations ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.schema_migrations FORCE ROW LEVEL SECURITY;
CREATE POLICY schema_migrations_tenant ON catalog.schema_migrations
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.schema_migrations TO wamn_app;

-- ---------------------------------------------------------------------------
-- Entities. `is_system` = platform-provided, structure-locked but extensible.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.entities (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    catalog_version int  NOT NULL,
    entity_id       text NOT NULL,
    name            text NOT NULL,
    is_system       boolean NOT NULL DEFAULT false,
    label           text,
    description     text,
    PRIMARY KEY (tenant_id, catalog_id, catalog_version, entity_id),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.catalogs (tenant_id, catalog_id, version) ON DELETE CASCADE,
    UNIQUE (tenant_id, catalog_id, catalog_version, name)
);
ALTER TABLE catalog.entities ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.entities FORCE ROW LEVEL SECURITY;
CREATE POLICY entities_tenant ON catalog.entities
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.entities TO wamn_app;

-- ---------------------------------------------------------------------------
-- Fields. `type` is the FieldType as JSON — the exact shape crates/schema/model
-- emits (e.g. {"kind":"numeric","precision":12,"scale":3,"unit":"kg"}). The
-- crate is the single source of truth for type semantics; the DDL compiler
-- (3.2) interprets this jsonb via the wamn-catalog types rather than this schema
-- enumerating every variant as columns. `ordinal` preserves field order.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.fields (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    catalog_version int  NOT NULL,
    entity_id       text NOT NULL,
    field_id        text NOT NULL,
    ordinal         int  NOT NULL,
    name            text NOT NULL,
    type            jsonb NOT NULL,
    nullable        boolean NOT NULL DEFAULT false,
    default_json    jsonb,
    sensitive       boolean NOT NULL DEFAULT false,
    is_system       boolean NOT NULL DEFAULT false,
    label           text,
    description     text,
    PRIMARY KEY (tenant_id, catalog_id, catalog_version, entity_id, field_id),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version, entity_id)
        REFERENCES catalog.entities (tenant_id, catalog_id, catalog_version, entity_id) ON DELETE CASCADE,
    UNIQUE (tenant_id, catalog_id, catalog_version, entity_id, name)
);
ALTER TABLE catalog.fields ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.fields FORCE ROW LEVEL SECURITY;
CREATE POLICY fields_tenant ON catalog.fields
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.fields TO wamn_app;

-- ---------------------------------------------------------------------------
-- Relations. Navigational metadata over the physical FKs (a Reference field is
-- the FK column itself). `cardinality` is one-to-many | many-to-many |
-- hierarchical; `through` is the join entity for many-to-many.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.relations (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    catalog_version int  NOT NULL,
    relation_id     text NOT NULL,
    name            text NOT NULL,
    cardinality     text NOT NULL,
    from_entity     text NOT NULL,
    to_entity       text NOT NULL,
    from_field      text,
    through_entity  text,
    description     text,
    PRIMARY KEY (tenant_id, catalog_id, catalog_version, relation_id),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.catalogs (tenant_id, catalog_id, version) ON DELETE CASCADE,
    CONSTRAINT relations_cardinality_check
        CHECK (cardinality IN ('one-to-many', 'many-to-many', 'hierarchical'))
);
ALTER TABLE catalog.relations ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.relations FORCE ROW LEVEL SECURITY;
CREATE POLICY relations_tenant ON catalog.relations
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.relations TO wamn_app;

-- ---------------------------------------------------------------------------
-- Secondary indexes. `fields` is the ordered list of field_ids covered.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.indexes (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    catalog_version int  NOT NULL,
    entity_id       text NOT NULL,
    index_name      text NOT NULL,
    fields          text[] NOT NULL,
    is_unique       boolean NOT NULL DEFAULT false,
    PRIMARY KEY (tenant_id, catalog_id, catalog_version, entity_id, index_name),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version, entity_id)
        REFERENCES catalog.entities (tenant_id, catalog_id, catalog_version, entity_id) ON DELETE CASCADE
);
ALTER TABLE catalog.indexes ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.indexes FORCE ROW LEVEL SECURITY;
CREATE POLICY indexes_tenant ON catalog.indexes
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.indexes TO wamn_app;

-- ---------------------------------------------------------------------------
-- Table-level constraints. `kind` is unique | check; `fields` carries the
-- covered field_ids for a unique constraint; `expression` the boolean check.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.constraints (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    catalog_version int  NOT NULL,
    entity_id       text NOT NULL,
    constraint_name text NOT NULL,
    kind            text NOT NULL,
    fields          text[],
    expression      text,
    PRIMARY KEY (tenant_id, catalog_id, catalog_version, entity_id, constraint_name),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version, entity_id)
        REFERENCES catalog.entities (tenant_id, catalog_id, catalog_version, entity_id) ON DELETE CASCADE,
    CONSTRAINT constraints_kind_check CHECK (kind IN ('unique', 'check'))
);
ALTER TABLE catalog.constraints ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.constraints FORCE ROW LEVEL SECURITY;
CREATE POLICY constraints_tenant ON catalog.constraints
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.constraints TO wamn_app;

-- ---------------------------------------------------------------------------
-- RLS access rules (3.5, crates/schema/compiler/src/rls). Per-entity access rules tied to
-- roles — row ownership, role command gates, custom per-role predicates —
-- authored against a catalog and compiled to Postgres RLS policies that layer
-- AS RESTRICTIVE on top of the 3.2 tenant floor. Each `rule` is the Rule JSON
-- (the crate is the source of truth for its semantics; the RLS compiler
-- interprets this jsonb via the wamn-rls types rather than this schema
-- enumerating every rule kind). These are the DEFINITIONS; the compiler emits
-- the CREATE POLICY statements applied to the project data tables. Not tied to
-- a specific catalog *version*: policies attach to the live schema.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.rls_policies (
    tenant_id  text NOT NULL CHECK (tenant_id <> ''),
    catalog_id text NOT NULL,
    policy_id  text NOT NULL,
    entity_id  text NOT NULL,
    rule       jsonb NOT NULL,
    PRIMARY KEY (tenant_id, catalog_id, policy_id)
);
ALTER TABLE catalog.rls_policies ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.rls_policies FORCE ROW LEVEL SECURITY;
CREATE POLICY rls_policies_tenant ON catalog.rls_policies
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.rls_policies TO wamn_app;

-- ---------------------------------------------------------------------------
-- Seed datasets (3.6, crates/schema/compiler/src/seed). Reference/fixture data for a catalog —
-- rows grouped by entity, referenced by symbolic key — authored once and
-- compiled to tenant-scoped, idempotent INSERTs against the generated tables
-- (deterministic uuidv5 ids keep re-seeds and test-host schema clones stable).
-- The `dataset` jsonb is the Dataset document (the crate is the source of truth
-- for its semantics); the compiler emits the INSERTs from it. These are the
-- DEFINITIONS, not the seeded rows themselves.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.seed_datasets (
    tenant_id  text NOT NULL CHECK (tenant_id <> ''),
    catalog_id text NOT NULL,
    dataset_id text NOT NULL,
    dataset    jsonb NOT NULL,
    PRIMARY KEY (tenant_id, catalog_id, dataset_id)
);
ALTER TABLE catalog.seed_datasets ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.seed_datasets FORCE ROW LEVEL SECURITY;
CREATE POLICY seed_datasets_tenant ON catalog.seed_datasets
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.seed_datasets TO wamn_app;

-- BEGIN COMPONENT LIBRARY STORAGE MIGRATION (wamn-0h0g.21.1)
-- ---------------------------------------------------------------------------
-- Immutable, catalog-versioned component admission facts. The byte validator
-- stores one logical operation per component digest, the exact import inventory
-- it audited, and normalized typed port/parameter JSON containing each schema's
-- RFC 8785 digest. Environment is intentionally absent: environment selects a
-- wiring pointer; it never changes what bytes or interface a catalog admitted.
--
-- `effects` (wamn-0h0g.21.9) is the validator's projection of `imports` onto the
-- authority packages that leave the host, grouped as
-- [{"package", "interfaces"}]. It is derived, never declared: an author cannot
-- claim fewer effects than the bytes import, and an empty array is the positive
-- statement that the component is pure. Its portable connection requirements
-- land in catalog.connection_requirements at (component_digest, store_alias),
-- which is why no alias column appears here.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.component_library (
    tenant_id          text NOT NULL CHECK (tenant_id <> ''),
    catalog_id         text NOT NULL CHECK (catalog_id <> ''),
    catalog_version    int NOT NULL CHECK (catalog_version > 0),
    component          text NOT NULL CHECK (component <> ''),
    interface_version  text NOT NULL CHECK (interface_version <> ''),
    operation          text NOT NULL CHECK (operation <> ''),
    component_digest   text NOT NULL
        CHECK (component_digest ~ '^sha256:[0-9a-f]{64}$'),
    imports            jsonb NOT NULL CHECK (jsonb_typeof(imports) = 'array'),
    imports_fingerprint text NOT NULL
        CHECK (imports_fingerprint ~ '^sha256:[0-9a-f]{64}$'),
    effects            jsonb NOT NULL CHECK (jsonb_typeof(effects) = 'array'),
    input_ports        jsonb NOT NULL CHECK (jsonb_typeof(input_ports) = 'array'),
    output_ports       jsonb NOT NULL CHECK (jsonb_typeof(output_ports) = 'array'),
    parameters         jsonb NOT NULL CHECK (jsonb_typeof(parameters) = 'array'),
    admitted_at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (
        tenant_id, catalog_id, catalog_version, component, interface_version
    ),
    CONSTRAINT component_library_one_operation_per_digest UNIQUE (
        tenant_id, catalog_id, catalog_version, component_digest
    ),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.catalogs (tenant_id, catalog_id, version)
);
ALTER TABLE catalog.component_library ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.component_library FORCE ROW LEVEL SECURITY;
CREATE POLICY component_library_tenant ON catalog.component_library
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.component_library TO wamn_app;
CREATE TRIGGER component_library_immutable
BEFORE UPDATE OR DELETE ON catalog.component_library
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
-- END COMPONENT LIBRARY STORAGE MIGRATION (wamn-0h0g.21.1)

-- BEGIN COMPONENT LIBRARY EFFECTS MIGRATION (wamn-0h0g.21.9)
-- Converge the derived effect projection onto a library installed before it
-- existed. On a fresh install the column is already present and every clause
-- below is a no-op; the DEFAULT exists only so the additive ALTER can land on
-- an immutable relation whose rows cannot be rewritten afterwards.
--
-- A row admitted before this migration therefore reads as '[]' — "pure" — which
-- its validator never derived. wamn-0h0g.21.10 refuses that claim rather than
-- rewriting it: `effects` is a total function of `imports`, and `imports` is the
-- half the OCI artifact config digest attests, so every reader re-derives the
-- projection from the row's own audited imports and refuses a row the two do not
-- agree on. A pre-migration component that imports nothing leaving the host
-- keeps its '[]' — now derived rather than asserted; one that does is
-- unpublishable until it is re-admitted through the validator, which is why no
-- backfill appears here. See wamn_catalog::verify_stored_effect_projection.
ALTER TABLE catalog.component_library
    ADD COLUMN IF NOT EXISTS effects jsonb NOT NULL DEFAULT '[]'::jsonb
        CHECK (jsonb_typeof(effects) = 'array');
ALTER TABLE catalog.component_library
    ALTER COLUMN effects DROP DEFAULT;
-- END COMPONENT LIBRARY EFFECTS MIGRATION (wamn-0h0g.21.9)

-- BEGIN WIRING STORAGE MIGRATION (wamn-0h0g.18.2)
-- ---------------------------------------------------------------------------
-- Wirings: the gated tenant graph over palette components (exe-model rev 4 R3,
-- "wirings are data"; wamn-0h0g.18.1). A wiring definition is an immutable
-- versioned row gated against ONE applied catalog version. Activation is the
-- operational, environment-scoped pointer confirming exactly one definition
-- hash, and every flip appends one provenance row.
--
-- The shape is deliberately the callable-flow attachment shape above: the same
-- join through `catalog_heads.applied_catalog_version`, the same "not current"
-- refusal, the same permanent tombstone on a removed id. Where it differs it is
-- because a wiring is NOT a release member: it is authored, gated and flipped on
-- the tenant's own cadence (R3's "two speeds"), so it carries its own version
-- and NAMES the catalog version it was gated against instead of being keyed by
-- the release that carries it.
--
-- wamn-0h0g.8.5.6: the row carries ONE identity. `gate_report_id` used to sit
-- beside `wiring_hash` as bare `text NOT NULL` with no foreign key to anything —
-- two identifiers for one fact on an immutable row — and it collapsed into the
-- hash, which is what the gate report now keys on.
--
-- The stored document is `WiringDocument` (crates/catalog/model): `entry` names
-- the node a delivery enters at, nodes reference `(component,
-- interface-version)` and may declare a `terminal` of `respond` or `emit`
-- (wamn-0h0g.18.5 — the authoring source of the router's verdict), edges connect
-- declared ports, parameters bind declared params, and the in-draft `cases`
-- array rides the document (the wamn-0h0g.18.4 ruling — cases attach to the
-- WIRING, not to a component). The document is persisted WHOLE in `graph_json`
-- and no column enumerates a field of it, which is why a document field is a
-- model change and never a migration. `wiring_hash` is the sha256 of that
-- document's RFC 8785 canonical bytes; it is not CHECK-derivable from the
-- jsonb column, which stores a parsed document rather than the exact bytes.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.wirings (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    wiring_id       text NOT NULL CHECK (wiring_id <> ''),
    version         int NOT NULL CHECK (version > 0),
    -- The applied catalog version this definition was gated against. Promotion
    -- is one integer comparison against the target's applied version; the FK
    -- makes the gated version a real row rather than an assertion.
    gated_catalog_version int NOT NULL CHECK (gated_catalog_version > 0),
    graph_json      jsonb NOT NULL CHECK (jsonb_typeof(graph_json) = 'object'),
    -- The gate report that certified this definition is keyed by this SAME hash
    -- (wamn-0h0g.8.5.6, ratified spec §2.1). A gate is effect-free, so its
    -- report is reproducible from the document and needs no identity of its own;
    -- see the section header for the second identifier this replaced.
    wiring_hash     text NOT NULL
        CHECK (wiring_hash ~ '^sha256:[0-9a-f]{64}$'),
    created_at      timestamptz NOT NULL DEFAULT now(),
    -- `catalog_id` is part of the identity, not an attribute: activation is
    -- keyed by (tenant, catalog, environment, wiring), so a wiring id shared by
    -- two catalogs must not resolve one catalog's pointer to the other's
    -- definition.
    PRIMARY KEY (tenant_id, catalog_id, wiring_id, version),
    FOREIGN KEY (tenant_id, catalog_id, gated_catalog_version)
        REFERENCES catalog.catalogs (tenant_id, catalog_id, version)
);
ALTER TABLE catalog.wirings ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.wirings FORCE ROW LEVEL SECURITY;
CREATE POLICY wirings_tenant ON catalog.wirings
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.wirings TO wamn_app;
CREATE TRIGGER wirings_immutable
BEFORE UPDATE ON catalog.wirings
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
CREATE TRIGGER wirings_delete_immutable
BEFORE DELETE ON catalog.wirings
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

-- A removed wiring id is permanently retired for this environment and cannot be
-- reused: the definition rows stay (they are immutable), so without this the
-- pointer could be re-enabled onto a definition the author deleted.
CREATE TABLE catalog.wiring_tombstones (
    tenant_id   text NOT NULL CHECK (tenant_id <> ''),
    catalog_id  text NOT NULL,
    environment text NOT NULL,
    wiring_id   text NOT NULL,
    removed_in_catalog_version int NOT NULL,
    removed_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, catalog_id, environment, wiring_id)
);
ALTER TABLE catalog.wiring_tombstones ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.wiring_tombstones FORCE ROW LEVEL SECURITY;
CREATE POLICY wiring_tombstones_tenant ON catalog.wiring_tombstones
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.wiring_tombstones TO wamn_app;

-- The env-scoped enabled pointer. The primary key IS the "exactly one
-- definition hash" rule: unlike an attachment id, a wiring id is the pointer's
-- own key, so one environment cannot hold two enabled hashes for one wiring and
-- the attachment template's second, kind-scoped refusal has no analogue here.
CREATE TABLE catalog.wiring_activation (
    tenant_id   text NOT NULL CHECK (tenant_id <> ''),
    catalog_id  text NOT NULL,
    environment text NOT NULL,
    wiring_id   text NOT NULL,
    confirmed_definition_hash text NOT NULL,
    enabled     boolean NOT NULL DEFAULT false,
    changed_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, catalog_id, environment, wiring_id)
);
ALTER TABLE catalog.wiring_activation ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.wiring_activation FORCE ROW LEVEL SECURITY;
CREATE POLICY wiring_activation_tenant ON catalog.wiring_activation
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.wiring_activation TO wamn_app;

CREATE FUNCTION catalog.validate_wiring_activation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_version int;
BEGIN
    IF NOT NEW.enabled THEN
        RETURN NEW;
    END IF;
    SELECT wiring.version
    INTO target_version
    FROM catalog.catalog_heads head
    JOIN catalog.wirings wiring
      ON wiring.tenant_id = head.tenant_id
     AND wiring.catalog_id = head.catalog_id
     AND wiring.gated_catalog_version = head.applied_catalog_version
    WHERE head.tenant_id = NEW.tenant_id
      AND head.catalog_id = NEW.catalog_id
      AND head.environment = NEW.environment
      AND wiring.wiring_id = NEW.wiring_id
      AND wiring.wiring_hash = NEW.confirmed_definition_hash
      AND NOT EXISTS (
          SELECT 1 FROM catalog.wiring_tombstones dead
          WHERE dead.tenant_id = NEW.tenant_id
            AND dead.catalog_id = NEW.catalog_id
            AND dead.environment = NEW.environment
            AND dead.wiring_id = NEW.wiring_id
      );
    IF target_version IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '23503',
            MESSAGE = 'wiring-definition-not-current';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER wiring_activation_valid
BEFORE INSERT OR UPDATE ON catalog.wiring_activation
FOR EACH ROW EXECUTE FUNCTION catalog.validate_wiring_activation();

-- The doorbell (wamn-0h0g.18.2). Activation is seconds, not a rollout: serving
-- processes hold a version-keyed resolution cache, so a flip has to reach them
-- without a restart or a poll.
--
-- `pg_notify` from inside the flip is delivered only when that transaction
-- COMMITS, which is why the doorbell is Postgres and not the control-plane NATS
-- the dispatcher rings: a NATS publish cannot be in the flip's transaction, so
-- it either announces a flip that then rolled back or is lost with no re-hint
-- sweep to recover it (the waker's loss tolerance is bought by the dispatcher
-- re-hinting every due row, and pointer flips have no such sweep).
--
-- It rides the POINTER, not the provenance row: the pointer is what the read
-- path serves, so binding the ring to it means no writer can move what is served
-- without ringing. `AFTER INSERT OR UPDATE` because the first activation is an
-- INSERT and every later flip — including the rollback to the prior version — is
-- an UPDATE of the same key.
--
-- Failure mode, and the listener's obligation: PostgreSQL delivers to sessions
-- LISTENing at commit time and queues nothing for an absent one, so a listener
-- that reconnects has missed an unknown set of flips and must drop its whole
-- cache rather than trust the gap.
CREATE FUNCTION catalog.notify_wiring_activation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM pg_notify(
        'wamn_wiring_activation',
        json_build_object(
            'tenant-id', NEW.tenant_id,
            'catalog-id', NEW.catalog_id,
            'environment', NEW.environment,
            'wiring-id', NEW.wiring_id,
            'enabled', NEW.enabled,
            'confirmed-definition-hash', NEW.confirmed_definition_hash
        )::text
    );
    RETURN NULL;
END
$$;
CREATE TRIGGER wiring_activation_doorbell
AFTER INSERT OR UPDATE ON catalog.wiring_activation
FOR EACH ROW EXECUTE FUNCTION catalog.notify_wiring_activation();

-- Append-only provenance: one row per flip. `source_environment` is the promote
-- half of the provenance fact — the env a wiring was proved green in; a local
-- flip has no source and leaves it NULL.
--
-- No report id is carried here at all (wamn-0h0g.8.5.6). The `source_gate_report_id`
-- that used to pair with `source_environment` held the SOURCE row's
-- `gate_report_id`, and promotion copies a wiring document byte-for-byte, so
-- once the report keys on `wiring_hash` that value is `confirmed_definition_hash`
-- spelled twice on one row — exactly the second copy this table's own rule
-- forbids, because it could disagree with the definition it claims to certify.
CREATE TABLE catalog.wiring_activation_events (
    event_seq   bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id   text NOT NULL CHECK (tenant_id <> ''),
    catalog_id  text NOT NULL,
    environment text NOT NULL,
    wiring_id   text NOT NULL,
    enabled     boolean NOT NULL,
    confirmed_definition_hash text NOT NULL,
    source_environment    text,
    changed_at  timestamptz NOT NULL DEFAULT now(),
    changed_by  text NOT NULL,
    reason      text NOT NULL,
    CONSTRAINT wiring_activation_events_promote_provenance CHECK (
        source_environment IS NULL OR source_environment <> ''
    )
);
ALTER TABLE catalog.wiring_activation_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.wiring_activation_events FORCE ROW LEVEL SECURITY;
CREATE POLICY wiring_activation_events_tenant ON catalog.wiring_activation_events
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.wiring_activation_events TO wamn_app;
-- Append-only against the owning role too, not only against the grants: the
-- same idiom used for provenance rows, because a rewritten provenance row is
-- worse than an absent one.
CREATE TRIGGER wiring_activation_events_immutable
BEFORE UPDATE OR DELETE ON catalog.wiring_activation_events
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
-- END WIRING STORAGE MIGRATION (wamn-0h0g.18.2)

-- BEGIN RELEASE COMPONENT MEMBERSHIP MIGRATION (wamn-0h0g.25.2)
-- ---------------------------------------------------------------------------
-- The immutable component closure of one release, recorded at the grain that
-- produced it: an exact wiring version and an admitted component digest. The
-- component name and interface version are deliberately not copied here;
-- `catalog.component_library` is immutable and the digest foreign key resolves
-- those facts without creating a second component carrier. A wiring with the
-- same component at several nodes contributes one row, while two wirings using
-- the same digest remain separately attributable.
--
-- This is the format-2 serving-manifest source. Legacy flow-era membership and
-- execution plans are never converted into these rows: a release is either
-- minted from current wiring/component facts or has no component closure.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.release_components (
    tenant_id        text NOT NULL CHECK (tenant_id <> ''),
    catalog_id       text NOT NULL CHECK (catalog_id <> ''),
    catalog_version  int NOT NULL CHECK (catalog_version > 0),
    wiring_id        text NOT NULL CHECK (wiring_id <> ''),
    wiring_version   int NOT NULL CHECK (wiring_version > 0),
    component_digest text NOT NULL
        CHECK (component_digest ~ '^sha256:[0-9a-f]{64}$'),
    PRIMARY KEY (
        tenant_id, catalog_id, catalog_version,
        wiring_id, wiring_version, component_digest
    ),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.releases
            (tenant_id, catalog_id, catalog_version),
    FOREIGN KEY (tenant_id, catalog_id, wiring_id, wiring_version)
        REFERENCES catalog.wirings (tenant_id, catalog_id, wiring_id, version),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version, component_digest)
        REFERENCES catalog.component_library
            (tenant_id, catalog_id, catalog_version, component_digest)
);
ALTER TABLE catalog.release_components ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.release_components FORCE ROW LEVEL SECURITY;
CREATE POLICY release_components_tenant ON catalog.release_components
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.release_components TO wamn_app;
CREATE TRIGGER release_components_immutable
BEFORE UPDATE OR DELETE ON catalog.release_components
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

-- The complete v2 source freeze. Component membership remains relational above
-- for promotion coverage; these exact canonical bytes additionally bind every
-- attachment and registration fact so retrying one release coordinate cannot
-- silently mint another serving identity.
CREATE TABLE catalog.release_manifest_v2_snapshots (
    tenant_id        text NOT NULL CHECK (tenant_id <> ''),
    catalog_id       text NOT NULL CHECK (catalog_id <> ''),
    catalog_version  int NOT NULL CHECK (catalog_version > 0),
    manifest_digest  text NOT NULL
        CHECK (manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    canonical_bytes  bytea NOT NULL CHECK (octet_length(canonical_bytes) > 0),
    PRIMARY KEY (tenant_id, catalog_id, catalog_version),
    CONSTRAINT release_manifest_v2_snapshots_exact_hash CHECK (
        manifest_digest = 'sha256:' || encode(sha256(canonical_bytes), 'hex')
    ),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.releases
            (tenant_id, catalog_id, catalog_version)
);
ALTER TABLE catalog.release_manifest_v2_snapshots ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.release_manifest_v2_snapshots FORCE ROW LEVEL SECURITY;
CREATE POLICY release_manifest_v2_snapshots_tenant
ON catalog.release_manifest_v2_snapshots
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.release_manifest_v2_snapshots TO wamn_app;
CREATE TRIGGER release_manifest_v2_snapshots_immutable
BEFORE UPDATE OR DELETE ON catalog.release_manifest_v2_snapshots
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

-- Take the same release-coordinate lock as the mint before checking the seal.
-- That makes a concurrent out-of-band INSERT wait for an in-flight mint's
-- snapshot and refuse after it commits, rather than slipping into its closure.
CREATE FUNCTION catalog.guard_release_component_insert()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM 1
    FROM catalog.releases
    WHERE tenant_id = NEW.tenant_id
      AND catalog_id = NEW.catalog_id
      AND catalog_version = NEW.catalog_version
    FOR UPDATE;

    IF EXISTS (
        SELECT 1
        FROM catalog.release_manifest_v2_snapshots
        WHERE tenant_id = NEW.tenant_id
          AND catalog_id = NEW.catalog_id
          AND catalog_version = NEW.catalog_version
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'release-component-membership-frozen';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER release_components_snapshot_seal
BEFORE INSERT ON catalog.release_components
FOR EACH ROW EXECUTE FUNCTION catalog.guard_release_component_insert();
-- END RELEASE COMPONENT MEMBERSHIP MIGRATION (wamn-0h0g.25.2)

-- ---------------------------------------------------------------------------
-- Event registrations (EVT-REG, D19 v3 §5, crates/events/registration). One row per
-- registration: a subscribing flow's declaration of WHICH entity's row events it
-- wants (`entity_id`), WHICH ops, and an optional condition filter. The
-- materializer (crates/... l5i9.17) is the
-- consumer — a durable consumer per registration, condition evaluated there
-- (hot-editable). Managed through the minimal CRUD surface in crates/data/entity-access
-- (`registration` module); the editor panel lands later (EVT-TRIGGER-UX).
--
-- `entity_id` is the stable catalog ENTITY ID, not a table name, so a table
-- rename never orphans a registration (EVT-OIDMAP, wamn-l5i9.11); it matches the
-- CDC envelope's `entity` segment. It is a DENORMALIZED column — the full
-- declaration is the `registration` jsonb (crates/events/registration is the source
-- of truth for its semantics; the DB does not enumerate ops/condition as
-- columns) — so 11.8 impact analysis (wamn-wvb) can enumerate "which
-- registrations reference entity X" without opening every document, and the
-- materializer's per-entity sweep is indexed (`event_registrations_by_entity`).
-- Like catalog.rls_policies, registrations attach to the LIVE catalog, not a
-- specific catalog VERSION (a registration is hot-editable), so there is no
-- catalog_version column or version FK.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.event_registrations (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    registration_id text NOT NULL,
    flow_id         text NOT NULL,
    entity_id       text NOT NULL,
    registration    jsonb NOT NULL,
    PRIMARY KEY (tenant_id, catalog_id, registration_id)
);
ALTER TABLE catalog.event_registrations ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.event_registrations FORCE ROW LEVEL SECURITY;
CREATE POLICY event_registrations_tenant ON catalog.event_registrations
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.event_registrations TO wamn_app;
-- wamn-0h0g.12.29: callable-flow admission locks the live registration with
-- `FOR KEY SHARE` as wamn_app, and PostgreSQL demands UPDATE on at least one
-- column for ANY row-locking clause. `tenant_id` is the only column whose
-- FORCE-RLS WITH CHECK admits nothing but the value already in the row, so this
-- grant buys the lock and carries no semantic rewrite authority.
GRANT UPDATE (tenant_id) ON catalog.event_registrations TO wamn_app;
-- Impact-analysis (wamn-wvb) + materializer lookup by the rename-proof entity id.
CREATE INDEX event_registrations_by_entity
    ON catalog.event_registrations (tenant_id, catalog_id, entity_id);

-- Closes the BEGIN at the head of the file (wamn-jnms).
COMMIT;
