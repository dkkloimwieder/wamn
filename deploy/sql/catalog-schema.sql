-- Metadata catalog storage schema (3.1). The tables that PERSIST the catalog
-- model defined by crates/wamn-catalog — entities, fields, relations, indexes,
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
-- Security shape mirrors the rest of the platform (s2/s3): one application role
-- (wamn_app, not owner, no BYPASSRLS) and tenant separation purely via the
-- `app.tenant` claim the wamn:postgres plugin injects with SET LOCAL. Every
-- table FORCEs RLS keyed on NULLIF(current_setting('app.tenant', true), ''),
-- which is NULL (=> zero rows) when no claim was injected — Postgres resets a
-- custom GUC to '' (not NULL) after SET LOCAL, and CHECK (tenant_id <> '')
-- forbids a ''-tenant row, so an empty claim matches nothing structurally.
-- (In production the catalog may live
-- in the control plane rather than a project DB; the tenant-scoped RLS shape is
-- the same either way.)

CREATE SCHEMA catalog AUTHORIZATION postgres;
GRANT USAGE ON SCHEMA catalog TO wamn_app;

-- ---------------------------------------------------------------------------
-- Catalog header: one row per (catalog_id, version) — the unit versioned and
-- promoted between environments (3.4, crates/wamn-schema). `schema_version` is
-- the catalog-MODEL format version (crates/wamn-catalog SCHEMA_VERSION),
-- distinct from `version`.
--
-- Lifecycle (3.4): `state` carries the draft -> staged -> applied -> superseded
-- lifecycle (generalizing the earlier `active` boolean); its values are exactly
-- crates/wamn-schema State::as_sql, tied to the crate by a test. `environment`
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
-- `document` is the full catalog JSON (crates/wamn-catalog Catalog) for this
-- version — written by the migration engine (2.5, crates/wamn-migrate) as the
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
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.catalogs TO wamn_app;

-- Single-applied invariant: exactly one live version per (catalog, environment).
CREATE UNIQUE INDEX catalogs_one_applied_per_env
    ON catalog.catalogs (tenant_id, catalog_id, environment)
    WHERE state = 'applied';

-- ---------------------------------------------------------------------------
-- Migration history (2.5, crates/wamn-migrate). One IMMUTABLE row per applied
-- migration — the versioned, forward-only apply journal the migration engine
-- writes inside the SAME transaction as the DDL + the lifecycle advance. A row
-- records the (from -> to) version step, whether it was destructive (so a
-- confirmed backup checkpoint was required), the operation count, and a checksum
-- of the applied DDL script (integrity/audit). `from_version` is NULL for the
-- first materialization of a catalog. Forward-only: the PK forbids recording the
-- same (catalog, environment, to_version) twice; wamn_app is granted SELECT +
-- INSERT only (no UPDATE/DELETE) so history is append-only.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.schema_migrations (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    environment     text NOT NULL,
    from_version    int,
    to_version      int  NOT NULL,
    confirmation    text NOT NULL,
    statement_count int  NOT NULL,
    destructive     boolean NOT NULL DEFAULT false,
    checksum        text NOT NULL,
    applied_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, catalog_id, environment, to_version),
    -- `environment` is a validated slug (D18, wamn-8df.3) — no closed CHECK.
    CONSTRAINT schema_migrations_confirmation_check
        CHECK (confirmation IN ('none', 'confirmed-with-backup'))
);
ALTER TABLE catalog.schema_migrations ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.schema_migrations FORCE ROW LEVEL SECURITY;
CREATE POLICY schema_migrations_tenant ON catalog.schema_migrations
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT ON catalog.schema_migrations TO wamn_app;

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
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.entities TO wamn_app;

-- ---------------------------------------------------------------------------
-- Fields. `type` is the FieldType as JSON — the exact shape crates/wamn-catalog
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
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.fields TO wamn_app;

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
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.relations TO wamn_app;

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
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.indexes TO wamn_app;

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
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.constraints TO wamn_app;

-- ---------------------------------------------------------------------------
-- RLS access rules (3.5, crates/wamn-rls). Per-entity access rules tied to
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
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.rls_policies TO wamn_app;

-- ---------------------------------------------------------------------------
-- Seed datasets (3.6, crates/wamn-seed). Reference/fixture data for a catalog —
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
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.seed_datasets TO wamn_app;
