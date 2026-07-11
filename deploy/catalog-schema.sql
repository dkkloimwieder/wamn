-- Metadata catalog storage schema (3.1). The tables that PERSIST the catalog
-- model defined by crates/wamn-catalog — entities, fields, relations, indexes,
-- and constraints — as versioned, tenant-scoped rows.
--
-- This is NOT the per-project *data* schema: the DDL compiler (3.2) reads these
-- rows and emits the actual project tables (`receipts`, `quality_holds`, ...).
-- These tables hold the *definitions* of those tables.
--
-- STANDALONE ARTIFACT: this file is deliberately NOT included by
-- deploy/postgres-init.sql. It is the persistence target the DDL compiler (3.2)
-- and the catalog-API-first POC build (POC-DM1) wire into a project database;
-- shipping it here keeps the 3.1 model and its storage shape reviewable in one
-- place without touching the S2–S6 gate fixtures.
--
-- Security shape mirrors the rest of the platform (s2/s3): one application role
-- (wamn_app, not owner, no BYPASSRLS) and tenant separation purely via the
-- `app.tenant` claim the wamn:postgres plugin injects with SET LOCAL. Every
-- table FORCEs RLS keyed on current_setting('app.tenant', true), which is NULL
-- (=> zero rows) when no claim was injected. (In production the catalog may live
-- in the control plane rather than a project DB; the tenant-scoped RLS shape is
-- the same either way.)

CREATE SCHEMA catalog AUTHORIZATION postgres;
GRANT USAGE ON SCHEMA catalog TO wamn_app;

-- ---------------------------------------------------------------------------
-- Catalog header: one row per (catalog_id, version) — the unit versioned and
-- promoted between environments (3.4). `active` flips like s3.flows.active:
-- exactly one version is the applied one. `schema_version` is the catalog-MODEL
-- format version (crates/wamn-catalog SCHEMA_VERSION), distinct from `version`.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.catalogs (
    tenant_id      text NOT NULL,
    catalog_id     text NOT NULL,
    version        int  NOT NULL,
    schema_version text NOT NULL,
    name           text,
    active         boolean NOT NULL DEFAULT false,
    PRIMARY KEY (tenant_id, catalog_id, version)
);
ALTER TABLE catalog.catalogs ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.catalogs FORCE ROW LEVEL SECURITY;
CREATE POLICY catalogs_tenant ON catalog.catalogs
    USING (tenant_id = current_setting('app.tenant', true))
    WITH CHECK (tenant_id = current_setting('app.tenant', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.catalogs TO wamn_app;

-- ---------------------------------------------------------------------------
-- Entities. `is_system` = platform-provided, structure-locked but extensible.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.entities (
    tenant_id       text NOT NULL,
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
    USING (tenant_id = current_setting('app.tenant', true))
    WITH CHECK (tenant_id = current_setting('app.tenant', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.entities TO wamn_app;

-- ---------------------------------------------------------------------------
-- Fields. `type` is the FieldType as JSON — the exact shape crates/wamn-catalog
-- emits (e.g. {"kind":"numeric","precision":12,"scale":3,"unit":"kg"}). The
-- crate is the single source of truth for type semantics; the DDL compiler
-- (3.2) interprets this jsonb via the wamn-catalog types rather than this schema
-- enumerating every variant as columns. `ordinal` preserves field order.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.fields (
    tenant_id       text NOT NULL,
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
    USING (tenant_id = current_setting('app.tenant', true))
    WITH CHECK (tenant_id = current_setting('app.tenant', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.fields TO wamn_app;

-- ---------------------------------------------------------------------------
-- Relations. Navigational metadata over the physical FKs (a Reference field is
-- the FK column itself). `cardinality` is one-to-many | many-to-many |
-- hierarchical; `through` is the join entity for many-to-many.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.relations (
    tenant_id       text NOT NULL,
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
    USING (tenant_id = current_setting('app.tenant', true))
    WITH CHECK (tenant_id = current_setting('app.tenant', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.relations TO wamn_app;

-- ---------------------------------------------------------------------------
-- Secondary indexes. `fields` is the ordered list of field_ids covered.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.indexes (
    tenant_id       text NOT NULL,
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
    USING (tenant_id = current_setting('app.tenant', true))
    WITH CHECK (tenant_id = current_setting('app.tenant', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.indexes TO wamn_app;

-- ---------------------------------------------------------------------------
-- Table-level constraints. `kind` is unique | check; `fields` carries the
-- covered field_ids for a unique constraint; `expression` the boolean check.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.constraints (
    tenant_id       text NOT NULL,
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
    USING (tenant_id = current_setting('app.tenant', true))
    WITH CHECK (tenant_id = current_setting('app.tenant', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.constraints TO wamn_app;
