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

-- ---------------------------------------------------------------------------
-- Immutable flow artifacts and catalog-release membership (FLOW-SPEC §5.1).
--
-- A flow version is content-addressed once. `register_flow_artifact` makes an
-- identical retry a no-op and gives a stable named conflict for different
-- content at the same `(tenant, flow, version)`. Direct UPDATE/DELETE is
-- rejected by the database, including for the owning role.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.flow_artifacts (
    tenant_id             text NOT NULL CHECK (tenant_id <> ''),
    flow_id               text NOT NULL,
    flow_version          int  NOT NULL CHECK (flow_version > 0),
    schema_version        text NOT NULL,
    graph_json            jsonb NOT NULL,
    graph_hash            text NOT NULL,
    artifact_hash         text NOT NULL,
    -- Nullable by design: only an authenticated application handler may
    -- supply human-principal provenance. Operator/service publication leaves
    -- this absent rather than attributing SESSION_USER to a human author.
    verified_author_principal text
        CHECK (verified_author_principal IS NULL OR verified_author_principal <> ''),
    created_at             timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, flow_id, flow_version)
);
ALTER TABLE catalog.flow_artifacts ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.flow_artifacts FORCE ROW LEVEL SECURITY;
CREATE POLICY flow_artifacts_tenant ON catalog.flow_artifacts
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.flow_artifacts TO wamn_app;
GRANT SELECT ON catalog.flow_artifacts TO wamn_scenario_author;

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

CREATE TRIGGER flow_artifacts_immutable
BEFORE UPDATE ON catalog.flow_artifacts
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
CREATE TRIGGER flow_artifacts_delete_immutable
BEFORE DELETE ON catalog.flow_artifacts
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

CREATE OR REPLACE FUNCTION catalog.register_flow_artifact(
    p_tenant_id text,
    p_flow_id text,
    p_flow_version int,
    p_schema_version text,
    p_graph_json jsonb,
    p_graph_hash text,
    p_artifact_hash text
)
RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog.flow_artifacts (
        tenant_id, flow_id, flow_version, schema_version, graph_json,
        graph_hash, artifact_hash
    )
    VALUES (
        p_tenant_id, p_flow_id, p_flow_version, p_schema_version, p_graph_json,
        p_graph_hash, p_artifact_hash
    )
    ON CONFLICT (tenant_id, flow_id, flow_version) DO NOTHING;

    IF NOT EXISTS (
        SELECT 1
        FROM catalog.flow_artifacts
        WHERE tenant_id = p_tenant_id
          AND flow_id = p_flow_id
          AND flow_version = p_flow_version
          AND schema_version = p_schema_version
          AND graph_json = p_graph_json
          AND graph_hash = p_graph_hash
          AND artifact_hash = p_artifact_hash
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = '23505',
            MESSAGE = 'flow-version-content-conflict';
    END IF;
END
$$;
REVOKE ALL ON FUNCTION catalog.register_flow_artifact(
    text, text, int, text, jsonb, text, text
) FROM PUBLIC;

CREATE TABLE catalog.execution_bundles (
    tenant_id              text NOT NULL CHECK (tenant_id <> ''),
    execution_bundle_hash  text NOT NULL
        CHECK (execution_bundle_hash ~ '^sha256:[0-9a-f]{64}$'),
    format_version         text NOT NULL CHECK (format_version = '0.1'),
    exact_bytes            bytea NOT NULL,
    byte_length            int NOT NULL
        CHECK (byte_length = octet_length(exact_bytes)),
    created_at             timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, execution_bundle_hash),
    CONSTRAINT execution_bundles_exact_hash CHECK (
        execution_bundle_hash = 'sha256:' || encode(sha256(exact_bytes), 'hex')
    )
);
ALTER TABLE catalog.execution_bundles ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.execution_bundles FORCE ROW LEVEL SECURITY;
CREATE POLICY execution_bundles_tenant ON catalog.execution_bundles
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.execution_bundles TO wamn_app;
GRANT SELECT, INSERT ON catalog.execution_bundles TO wamn_scenario_author;
CREATE TRIGGER execution_bundles_immutable
BEFORE UPDATE OR DELETE ON catalog.execution_bundles
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

-- The release identity row: one row per (tenant, catalog, version). It is the
-- idempotency anchor a republication conflicts on, the provenance record of who
-- published, and the foreign-key root that makes "member of a release that does
-- not exist" unrepresentable. Membership is NOT stored here: it is row-per-member
-- in catalog.release_flows, which is append-only, so a release may gain a member
-- but never change or lose one (wamn-0h0g.15.159).
CREATE TABLE catalog.release_manifests (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    catalog_version int  NOT NULL,
    -- Same provenance rule as flow_artifacts: never manufacture a publisher
    -- identity from the database/service login.
    verified_publisher_principal text
        CHECK (verified_publisher_principal IS NULL OR verified_publisher_principal <> ''),
    PRIMARY KEY (tenant_id, catalog_id, catalog_version),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.catalogs (tenant_id, catalog_id, version)
);
ALTER TABLE catalog.release_manifests ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.release_manifests FORCE ROW LEVEL SECURITY;
CREATE POLICY release_manifests_tenant ON catalog.release_manifests
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.release_manifests TO wamn_app;
GRANT SELECT ON catalog.release_manifests TO wamn_scenario_author;
CREATE TRIGGER release_manifests_immutable
BEFORE UPDATE ON catalog.release_manifests
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
CREATE TRIGGER release_manifests_delete_immutable
BEFORE DELETE ON catalog.release_manifests
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

-- BEGIN DISPOSITION PROVENANCE STORAGE MIGRATION (wamn-4u7p.42)
-- Additive upgrade for catalogs provisioned before verified publication
-- provenance existed. Existing rows deliberately remain NULL/unverified.
ALTER TABLE catalog.flow_artifacts
    ADD COLUMN IF NOT EXISTS verified_author_principal text;
ALTER TABLE catalog.release_manifests
    ADD COLUMN IF NOT EXISTS verified_publisher_principal text;
ALTER TABLE catalog.flow_artifacts
    DROP CONSTRAINT IF EXISTS flow_artifacts_verified_author_principal_check;
ALTER TABLE catalog.flow_artifacts
    ADD CONSTRAINT flow_artifacts_verified_author_principal_check
    CHECK (verified_author_principal IS NULL OR verified_author_principal <> '');
ALTER TABLE catalog.release_manifests
    DROP CONSTRAINT IF EXISTS release_manifests_verified_publisher_principal_check;
ALTER TABLE catalog.release_manifests
    ADD CONSTRAINT release_manifests_verified_publisher_principal_check
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
--
-- The membership half of that literal did not vanish, it moved up a tier: the
-- ctl preflights (services/ctl/src/publish_catalog.rs and copy_project_env.rs)
-- now derive the member set from catalog.release_flows and raise the SAME
-- literal on a mismatch. Do not re-add a membership RAISE below; under
-- row-per-member truth the primary key refuses a CHANGED member, the
-- UPDATE/DELETE immutability triggers refuse a REMOVED one, and an ADDED one is
-- permitted by ruling.
CREATE FUNCTION catalog.register_release_manifest(
    p_tenant_id text,
    p_catalog_id text,
    p_catalog_version int
)
RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog.release_manifests (
        tenant_id, catalog_id, catalog_version
    )
    VALUES (p_tenant_id, p_catalog_id, p_catalog_version)
    ON CONFLICT (tenant_id, catalog_id, catalog_version) DO NOTHING;

    IF NOT EXISTS (
        SELECT 1
        FROM catalog.release_manifests
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

CREATE TABLE catalog.release_flows (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    catalog_version int  NOT NULL,
    flow_id         text NOT NULL,
    flow_version    int  NOT NULL,
    execution_bundle_hash text NOT NULL,
    PRIMARY KEY (tenant_id, catalog_id, catalog_version, flow_id),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.release_manifests (tenant_id, catalog_id, catalog_version),
    FOREIGN KEY (tenant_id, flow_id, flow_version)
        REFERENCES catalog.flow_artifacts (tenant_id, flow_id, flow_version),
    CONSTRAINT release_flows_execution_bundle_hash_check
        CHECK (execution_bundle_hash ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT release_flows_execution_bundle_fk
        FOREIGN KEY (tenant_id, execution_bundle_hash)
        REFERENCES catalog.execution_bundles (tenant_id, execution_bundle_hash)
);
CREATE INDEX release_flows_execution_bundle
    ON catalog.release_flows (tenant_id, execution_bundle_hash);
ALTER TABLE catalog.release_flows ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.release_flows FORCE ROW LEVEL SECURITY;
CREATE POLICY release_flows_tenant ON catalog.release_flows
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.release_flows TO wamn_app;
GRANT SELECT ON catalog.release_flows TO wamn_scenario_author;
CREATE TRIGGER release_flows_immutable
BEFORE UPDATE ON catalog.release_flows
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
CREATE TRIGGER release_flows_delete_immutable
BEFORE DELETE ON catalog.release_flows
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

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

-- ---------------------------------------------------------------------------
-- Flow authoring workspace and immutable validated-draft artifacts (PLAN 6A,
-- wamn-ftfc.11). A workspace row is the one mutable optimistic head. Each
-- validation copies the exact head revision and edit timestamp into an
-- immutable, content-addressed row; later edits never move that execution pin.
-- Validating a draft does not register a release flow or mint release lineage.
-- ---------------------------------------------------------------------------
-- BEGIN AUTHORING DRAFT STORAGE MIGRATION (wamn-ftfc.11)
CREATE TABLE catalog.flow_drafts (
    tenant_id  text NOT NULL CHECK (tenant_id <> ''),
    draft_id   text NOT NULL CHECK (draft_id <> ''),
    flow_id    text NOT NULL CHECK (flow_id <> ''),
    revision   bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    -- The authoritative content: EXACTLY the bytes a client submitted, never
    -- reparsed. Deliberately unconstrained beyond `text` — an emptied or
    -- half-finished file is a legitimate intermediate edit, and refusing to
    -- store it would reintroduce the validation this column exists to remove.
    -- Nullable only for rows written before wamn-ftfc.2; every write sets it.
    definition text,
    -- RETIRED by wamn-ftfc.2 (expand phase). No draft write populates this any
    -- more; readers fall back to it only for pre-migration rows. Distinct from
    -- catalog.validated_flow_drafts.graph_json, which keeps parsed-document
    -- semantics because a validated artifact IS a parsed document.
    graph_json jsonb CHECK (jsonb_typeof(graph_json) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now(),
    edited_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, draft_id),
    CONSTRAINT flow_drafts_content_present
        CHECK (definition IS NOT NULL OR graph_json IS NOT NULL)
);
ALTER TABLE catalog.flow_drafts ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.flow_drafts FORCE ROW LEVEL SECURITY;
CREATE POLICY flow_drafts_tenant ON catalog.flow_drafts
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE ON catalog.flow_drafts TO wamn_scenario_author;

CREATE FUNCTION catalog.guard_flow_draft_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF (NEW.tenant_id, NEW.draft_id, NEW.flow_id, NEW.created_at)
       IS DISTINCT FROM
       (OLD.tenant_id, OLD.draft_id, OLD.flow_id, OLD.created_at)
       OR NEW.revision <> OLD.revision + 1
       OR NEW.edited_at <= OLD.edited_at THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'flow-draft-uncontrolled-update';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER flow_drafts_controlled_update
BEFORE UPDATE ON catalog.flow_drafts
FOR EACH ROW EXECUTE FUNCTION catalog.guard_flow_draft_update();
CREATE TRIGGER flow_drafts_delete_immutable
BEFORE DELETE ON catalog.flow_drafts
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

CREATE TABLE catalog.validated_flow_drafts (
    tenant_id                 text NOT NULL CHECK (tenant_id <> ''),
    draft_id                  text NOT NULL CHECK (draft_id <> ''),
    draft_revision            bigint NOT NULL CHECK (draft_revision > 0),
    draft_edited_at           timestamptz NOT NULL,
    draft_content_hash        text NOT NULL CHECK (draft_content_hash <> ''),
    catalog_id                text NOT NULL CHECK (catalog_id <> ''),
    catalog_version           int NOT NULL CHECK (catalog_version > 0),
    environment               text NOT NULL CHECK (environment <> ''),
    flow_id                   text NOT NULL CHECK (flow_id <> ''),
    runtime_flow_version      int NOT NULL CHECK (runtime_flow_version > 0),
    graph_json                jsonb NOT NULL CHECK (jsonb_typeof(graph_json) = 'object'),
    graph_hash                text NOT NULL CHECK (graph_hash <> ''),
    draft_artifact_hash       text NOT NULL CHECK (draft_artifact_hash <> ''),
    execution_bundle_hash     text NOT NULL
        CHECK (execution_bundle_hash ~ '^sha256:[0-9a-f]{64}$'),
    binding_base_artifact_hash text NOT NULL CHECK (binding_base_artifact_hash <> ''),
    validated_draft_hash      text NOT NULL CHECK (validated_draft_hash <> ''),
    validated_at              timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, validated_draft_hash),
    CONSTRAINT validated_flow_drafts_exact_pin UNIQUE (
        tenant_id, draft_id, draft_revision, draft_content_hash,
        catalog_id, catalog_version, environment,
        runtime_flow_version, draft_artifact_hash, execution_bundle_hash,
        binding_base_artifact_hash
    ),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.catalogs (tenant_id, catalog_id, version),
    FOREIGN KEY (tenant_id, execution_bundle_hash)
        REFERENCES catalog.execution_bundles (tenant_id, execution_bundle_hash)
);
ALTER TABLE catalog.validated_flow_drafts ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.validated_flow_drafts FORCE ROW LEVEL SECURITY;
CREATE POLICY validated_flow_drafts_tenant ON catalog.validated_flow_drafts
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.validated_flow_drafts TO wamn_app;
GRANT SELECT, INSERT ON catalog.validated_flow_drafts TO wamn_scenario_author;
CREATE TRIGGER validated_flow_drafts_immutable
BEFORE UPDATE OR DELETE ON catalog.validated_flow_drafts
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
-- END AUTHORING DRAFT STORAGE MIGRATION (wamn-ftfc.11)

-- ---------------------------------------------------------------------------
-- Callable-flow sources, attachments, and activation (FLOW-SPEC rev18
-- §§5.3-5.4, §7). Definitions are immutable release members. Activation is an
-- operational, environment-scoped overlay which confirms exactly one resolved
-- definition hash. Runtime readers use the authoritative views below.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.release_exposure_manifests (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    catalog_version int NOT NULL,
    definitions_json jsonb NOT NULL CHECK (jsonb_typeof(definitions_json) = 'object'),
    PRIMARY KEY (tenant_id, catalog_id, catalog_version),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.release_manifests (tenant_id, catalog_id, catalog_version)
);
ALTER TABLE catalog.release_exposure_manifests ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.release_exposure_manifests FORCE ROW LEVEL SECURITY;
CREATE POLICY release_exposure_manifests_tenant ON catalog.release_exposure_manifests
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.release_exposure_manifests TO wamn_app;
CREATE TRIGGER release_exposure_manifests_immutable
BEFORE UPDATE ON catalog.release_exposure_manifests
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
CREATE TRIGGER release_exposure_manifests_delete_immutable
BEFORE DELETE ON catalog.release_exposure_manifests
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

CREATE FUNCTION catalog.register_release_exposure_manifest(
    p_tenant_id text,
    p_catalog_id text,
    p_catalog_version int,
    p_definitions_json jsonb
)
RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog.release_exposure_manifests (
        tenant_id, catalog_id, catalog_version, definitions_json
    )
    VALUES (p_tenant_id, p_catalog_id, p_catalog_version, p_definitions_json)
    ON CONFLICT (tenant_id, catalog_id, catalog_version) DO NOTHING;
    IF NOT EXISTS (
        SELECT 1 FROM catalog.release_exposure_manifests
        WHERE tenant_id = p_tenant_id
          AND catalog_id = p_catalog_id
          AND catalog_version = p_catalog_version
          AND definitions_json = p_definitions_json
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '23505',
            MESSAGE = 'catalog-release-exposure-content-conflict';
    END IF;
END
$$;
REVOKE ALL ON FUNCTION catalog.register_release_exposure_manifest(
    text, text, int, jsonb
) FROM PUBLIC;

CREATE TABLE catalog.release_sources (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    catalog_version int NOT NULL,
    source_id       text NOT NULL,
    source_kind     text NOT NULL CHECK (source_kind IN ('auth', 'caller-policy', 'schedule')),
    definition_json jsonb NOT NULL CHECK (jsonb_typeof(definition_json) = 'object'),
    source_hash     text NOT NULL,
    PRIMARY KEY (tenant_id, catalog_id, catalog_version, source_id),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.release_exposure_manifests (tenant_id, catalog_id, catalog_version)
);
ALTER TABLE catalog.release_sources ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.release_sources FORCE ROW LEVEL SECURITY;
CREATE POLICY release_sources_tenant ON catalog.release_sources
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.release_sources TO wamn_app;
CREATE TRIGGER release_sources_immutable
BEFORE UPDATE ON catalog.release_sources
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
CREATE TRIGGER release_sources_delete_immutable
BEFORE DELETE ON catalog.release_sources
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

CREATE TABLE catalog.release_attachments (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    catalog_version int NOT NULL,
    attachment_id   text NOT NULL,
    attachment_kind text NOT NULL CHECK (attachment_kind IN ('http', 'internal', 'studio', 'cron')),
    flow_id         text NOT NULL,
    source_id       text NOT NULL,
    definition_hash text NOT NULL,
    definition_json jsonb NOT NULL CHECK (jsonb_typeof(definition_json) = 'object'),
    route_host      text,
    route_path      text,
    route_template  text,
    route_method    text,
    PRIMARY KEY (tenant_id, catalog_id, catalog_version, attachment_id),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version, flow_id)
        REFERENCES catalog.release_flows (tenant_id, catalog_id, catalog_version, flow_id),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version, source_id)
        REFERENCES catalog.release_sources (tenant_id, catalog_id, catalog_version, source_id),
    CONSTRAINT release_attachment_route_shape CHECK (
        (attachment_kind IN ('http', 'studio')
          AND route_host IS NOT NULL AND route_path IS NOT NULL
          AND route_template IS NOT NULL AND route_method IS NOT NULL)
        OR
        (attachment_kind IN ('internal', 'cron')
          AND route_host IS NULL AND route_path IS NULL
          AND route_template IS NULL AND route_method IS NULL)
    ),
    UNIQUE (
        tenant_id, catalog_id, catalog_version,
        route_host, route_template, route_method
    )
);
ALTER TABLE catalog.release_attachments ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.release_attachments FORCE ROW LEVEL SECURITY;
CREATE POLICY release_attachments_tenant ON catalog.release_attachments
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.release_attachments TO wamn_app;
CREATE TRIGGER release_attachments_immutable
BEFORE UPDATE ON catalog.release_attachments
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
CREATE TRIGGER release_attachments_delete_immutable
BEFORE DELETE ON catalog.release_attachments
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

CREATE TABLE catalog.attachment_tombstones (
    tenant_id     text NOT NULL CHECK (tenant_id <> ''),
    catalog_id    text NOT NULL,
    environment   text NOT NULL,
    attachment_id text NOT NULL,
    removed_in_catalog_version int NOT NULL,
    removed_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, catalog_id, environment, attachment_id)
);
ALTER TABLE catalog.attachment_tombstones ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.attachment_tombstones FORCE ROW LEVEL SECURITY;
CREATE POLICY attachment_tombstones_tenant ON catalog.attachment_tombstones
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.attachment_tombstones TO wamn_app;

CREATE TABLE catalog.attachment_activation (
    tenant_id     text NOT NULL CHECK (tenant_id <> ''),
    catalog_id    text NOT NULL,
    environment   text NOT NULL,
    attachment_id text NOT NULL,
    confirmed_definition_hash text NOT NULL,
    enabled       boolean NOT NULL DEFAULT false,
    changed_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, catalog_id, environment, attachment_id)
);
ALTER TABLE catalog.attachment_activation ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.attachment_activation FORCE ROW LEVEL SECURITY;
CREATE POLICY attachment_activation_tenant ON catalog.attachment_activation
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.attachment_activation TO wamn_app;

CREATE FUNCTION catalog.validate_attachment_activation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_kind text;
    target_flow text;
BEGIN
    IF NOT NEW.enabled THEN
        RETURN NEW;
    END IF;
    SELECT attachment.attachment_kind, attachment.flow_id
    INTO target_kind, target_flow
    FROM catalog.catalog_heads head
    JOIN catalog.release_attachments attachment
      ON attachment.tenant_id = head.tenant_id
     AND attachment.catalog_id = head.catalog_id
     AND attachment.catalog_version = head.applied_catalog_version
    WHERE head.tenant_id = NEW.tenant_id
      AND head.catalog_id = NEW.catalog_id
      AND head.environment = NEW.environment
      AND attachment.attachment_id = NEW.attachment_id
      AND attachment.definition_hash = NEW.confirmed_definition_hash
      AND NOT EXISTS (
          SELECT 1 FROM catalog.attachment_tombstones dead
          WHERE dead.tenant_id = NEW.tenant_id
            AND dead.catalog_id = NEW.catalog_id
            AND dead.environment = NEW.environment
            AND dead.attachment_id = NEW.attachment_id
      );
    IF target_kind IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '23503',
            MESSAGE = 'attachment-definition-not-current';
    END IF;
    IF target_kind IN ('internal', 'cron') AND EXISTS (
        SELECT 1
        FROM catalog.attachment_activation active
        JOIN catalog.catalog_heads head
          ON head.tenant_id = active.tenant_id
         AND head.catalog_id = active.catalog_id
         AND head.environment = active.environment
        JOIN catalog.release_attachments attachment
          ON attachment.tenant_id = head.tenant_id
         AND attachment.catalog_id = head.catalog_id
         AND attachment.catalog_version = head.applied_catalog_version
         AND attachment.attachment_id = active.attachment_id
         AND attachment.definition_hash = active.confirmed_definition_hash
        WHERE active.tenant_id = NEW.tenant_id
          AND active.catalog_id = NEW.catalog_id
          AND active.environment = NEW.environment
          AND active.enabled
          AND attachment.attachment_kind = target_kind
          AND attachment.flow_id = target_flow
          AND active.attachment_id <> NEW.attachment_id
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '23505',
            MESSAGE = 'multiple-enabled-' || target_kind || '-attachments';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER attachment_activation_valid
BEFORE INSERT OR UPDATE ON catalog.attachment_activation
FOR EACH ROW EXECUTE FUNCTION catalog.validate_attachment_activation();

CREATE TABLE catalog.attachment_activation_events (
    event_seq     bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id     text NOT NULL CHECK (tenant_id <> ''),
    catalog_id    text NOT NULL,
    environment   text NOT NULL,
    attachment_id text NOT NULL,
    enabled       boolean NOT NULL,
    confirmed_definition_hash text NOT NULL,
    changed_at    timestamptz NOT NULL DEFAULT now(),
    changed_by    text NOT NULL,
    reason        text NOT NULL
);
ALTER TABLE catalog.attachment_activation_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.attachment_activation_events FORCE ROW LEVEL SECURITY;
CREATE POLICY attachment_activation_events_tenant ON catalog.attachment_activation_events
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.attachment_activation_events TO wamn_app;

-- Apply a release's exposure overlay while the publisher holds catalog_heads.
-- Same-id/same-hash retains activation. New/changed definitions are disabled;
-- removed IDs are permanently tombstoned and cannot be reused.
CREATE FUNCTION catalog.apply_release_exposure(
    p_tenant_id text,
    p_catalog_id text,
    p_environment text,
    p_catalog_version int,
    p_changed_by text
)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    previous_version int;
    definition record;
    removed record;
    prior_hash text;
BEGIN
    SELECT applied_catalog_version INTO previous_version
    FROM catalog.catalog_heads
    WHERE tenant_id = p_tenant_id
      AND catalog_id = p_catalog_id
      AND environment = p_environment;

    IF EXISTS (
        SELECT 1
        FROM catalog.release_attachments next
        JOIN catalog.attachment_tombstones dead
          ON dead.tenant_id = next.tenant_id
         AND dead.catalog_id = next.catalog_id
         AND dead.environment = p_environment
         AND dead.attachment_id = next.attachment_id
        WHERE next.tenant_id = p_tenant_id
          AND next.catalog_id = p_catalog_id
          AND next.catalog_version = p_catalog_version
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '23505',
            MESSAGE = 'tombstoned-attachment-id';
    END IF;

    IF previous_version IS NOT NULL THEN
        FOR removed IN
            SELECT old.attachment_id, old.definition_hash
            FROM catalog.release_attachments old
            WHERE old.tenant_id = p_tenant_id
              AND old.catalog_id = p_catalog_id
              AND old.catalog_version = previous_version
              AND NOT EXISTS (
                SELECT 1 FROM catalog.release_attachments next
                WHERE next.tenant_id = old.tenant_id
                  AND next.catalog_id = old.catalog_id
                  AND next.catalog_version = p_catalog_version
                  AND next.attachment_id = old.attachment_id
              )
        LOOP
            INSERT INTO catalog.attachment_tombstones (
                tenant_id, catalog_id, environment, attachment_id,
                removed_in_catalog_version
            ) VALUES (
                p_tenant_id, p_catalog_id, p_environment,
                removed.attachment_id, p_catalog_version
            ) ON CONFLICT DO NOTHING;
            UPDATE catalog.attachment_activation
            SET enabled = false, changed_at = now()
            WHERE tenant_id = p_tenant_id
              AND catalog_id = p_catalog_id
              AND environment = p_environment
              AND attachment_id = removed.attachment_id;
            INSERT INTO catalog.attachment_activation_events (
                tenant_id, catalog_id, environment, attachment_id, enabled,
                confirmed_definition_hash, changed_by, reason
            ) VALUES (
                p_tenant_id, p_catalog_id, p_environment,
                removed.attachment_id, false, removed.definition_hash,
                p_changed_by, 'removed'
            );
        END LOOP;
    END IF;

    FOR definition IN
        SELECT attachment_id, definition_hash
        FROM catalog.release_attachments
        WHERE tenant_id = p_tenant_id
          AND catalog_id = p_catalog_id
          AND catalog_version = p_catalog_version
        ORDER BY attachment_id
    LOOP
        SELECT confirmed_definition_hash INTO prior_hash
        FROM catalog.attachment_activation
        WHERE tenant_id = p_tenant_id
          AND catalog_id = p_catalog_id
          AND environment = p_environment
          AND attachment_id = definition.attachment_id;
        IF prior_hash IS NULL THEN
            INSERT INTO catalog.attachment_activation (
                tenant_id, catalog_id, environment, attachment_id,
                confirmed_definition_hash, enabled
            ) VALUES (
                p_tenant_id, p_catalog_id, p_environment,
                definition.attachment_id, definition.definition_hash, false
            );
            INSERT INTO catalog.attachment_activation_events (
                tenant_id, catalog_id, environment, attachment_id, enabled,
                confirmed_definition_hash, changed_by, reason
            ) VALUES (
                p_tenant_id, p_catalog_id, p_environment,
                definition.attachment_id, false, definition.definition_hash,
                p_changed_by, 'new-definition'
            );
        ELSIF prior_hash <> definition.definition_hash THEN
            UPDATE catalog.attachment_activation
            SET confirmed_definition_hash = definition.definition_hash,
                enabled = false, changed_at = now()
            WHERE tenant_id = p_tenant_id
              AND catalog_id = p_catalog_id
              AND environment = p_environment
              AND attachment_id = definition.attachment_id;
            INSERT INTO catalog.attachment_activation_events (
                tenant_id, catalog_id, environment, attachment_id, enabled,
                confirmed_definition_hash, changed_by, reason
            ) VALUES (
                p_tenant_id, p_catalog_id, p_environment,
                definition.attachment_id, false, definition.definition_hash,
                p_changed_by, 'definition-changed'
            );
        END IF;
        prior_hash := NULL;
    END LOOP;
END
$$;
REVOKE ALL ON FUNCTION catalog.apply_release_exposure(
    text, text, text, int, text
) FROM PUBLIC;

CREATE FUNCTION catalog.set_attachment_activation(
    p_tenant_id text,
    p_catalog_id text,
    p_environment text,
    p_attachment_id text,
    p_confirmed_definition_hash text,
    p_enabled boolean,
    p_changed_by text,
    p_reason text
)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    target_kind text;
    target_flow text;
BEGIN
    IF EXISTS (
        SELECT 1 FROM catalog.attachment_tombstones
        WHERE tenant_id = p_tenant_id AND catalog_id = p_catalog_id
          AND environment = p_environment AND attachment_id = p_attachment_id
    ) OR NOT EXISTS (
        SELECT 1
        FROM catalog.catalog_heads head
        JOIN catalog.release_attachments attachment
          ON attachment.tenant_id = head.tenant_id
         AND attachment.catalog_id = head.catalog_id
         AND attachment.catalog_version = head.applied_catalog_version
        WHERE head.tenant_id = p_tenant_id
          AND head.catalog_id = p_catalog_id
          AND head.environment = p_environment
          AND attachment.attachment_id = p_attachment_id
          AND attachment.definition_hash = p_confirmed_definition_hash
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '23503',
            MESSAGE = 'attachment-definition-not-current';
    END IF;
    SELECT attachment.attachment_kind, attachment.flow_id
    INTO target_kind, target_flow
    FROM catalog.catalog_heads head
    JOIN catalog.release_attachments attachment
      ON attachment.tenant_id = head.tenant_id
     AND attachment.catalog_id = head.catalog_id
     AND attachment.catalog_version = head.applied_catalog_version
    WHERE head.tenant_id = p_tenant_id
      AND head.catalog_id = p_catalog_id
      AND head.environment = p_environment
      AND attachment.attachment_id = p_attachment_id;
    IF p_enabled AND target_kind IN ('internal', 'cron') AND EXISTS (
        SELECT 1 FROM catalog.active_attachments active
        WHERE active.tenant_id = p_tenant_id
          AND active.catalog_id = p_catalog_id
          AND active.environment = p_environment
          AND active.attachment_kind = target_kind
          AND active.flow_id = target_flow
          AND active.attachment_id <> p_attachment_id
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '23505',
            MESSAGE = 'multiple-enabled-' || target_kind || '-attachments';
    END IF;
    INSERT INTO catalog.attachment_activation (
        tenant_id, catalog_id, environment, attachment_id,
        confirmed_definition_hash, enabled
    ) VALUES (
        p_tenant_id, p_catalog_id, p_environment, p_attachment_id,
        p_confirmed_definition_hash, p_enabled
    )
    ON CONFLICT (tenant_id, catalog_id, environment, attachment_id)
    DO UPDATE SET confirmed_definition_hash = EXCLUDED.confirmed_definition_hash,
                  enabled = EXCLUDED.enabled, changed_at = now();
    INSERT INTO catalog.attachment_activation_events (
        tenant_id, catalog_id, environment, attachment_id, enabled,
        confirmed_definition_hash, changed_by, reason
    ) VALUES (
        p_tenant_id, p_catalog_id, p_environment, p_attachment_id, p_enabled,
        p_confirmed_definition_hash, p_changed_by, p_reason
    );
END
$$;
REVOKE ALL ON FUNCTION catalog.set_attachment_activation(
    text, text, text, text, text, boolean, text, text
) FROM PUBLIC;

CREATE VIEW catalog.attachment_definitions
WITH (security_invoker = true) AS
SELECT attachment.*, source.source_kind, source.definition_json AS source_definition_json
FROM catalog.release_attachments attachment
JOIN catalog.release_sources source
  USING (tenant_id, catalog_id, catalog_version, source_id);
GRANT SELECT ON catalog.attachment_definitions TO wamn_app;

CREATE VIEW catalog.active_attachments
WITH (security_invoker = true) AS
SELECT head.environment, definition.*
FROM catalog.catalog_heads head
JOIN catalog.attachment_definitions definition
  ON definition.tenant_id = head.tenant_id
 AND definition.catalog_id = head.catalog_id
 AND definition.catalog_version = head.applied_catalog_version
JOIN catalog.attachment_activation activation
  ON activation.tenant_id = head.tenant_id
 AND activation.catalog_id = head.catalog_id
 AND activation.environment = head.environment
 AND activation.attachment_id = definition.attachment_id
 AND activation.confirmed_definition_hash = definition.definition_hash
WHERE activation.enabled
  AND NOT EXISTS (
      SELECT 1 FROM catalog.attachment_tombstones dead
      WHERE dead.tenant_id = head.tenant_id
        AND dead.catalog_id = head.catalog_id
        AND dead.environment = head.environment
        AND dead.attachment_id = definition.attachment_id
  );
GRANT SELECT ON catalog.active_attachments TO wamn_app;

CREATE VIEW catalog.http_routes
WITH (security_invoker = true) AS
SELECT * FROM catalog.active_attachments
WHERE attachment_kind IN ('http', 'studio');
GRANT SELECT ON catalog.http_routes TO wamn_app;

CREATE VIEW catalog.cron_attachments
WITH (security_invoker = true) AS
SELECT * FROM catalog.active_attachments
WHERE attachment_kind = 'cron';
GRANT SELECT ON catalog.cron_attachments TO wamn_app;

-- BEGIN CONNECTION STORAGE MIGRATION (wamn-ko5r.6)
-- Portable requirements are artifact-owned. Every other record in this block
-- is environment-owned and therefore absent from artifact and bundle bytes.
CREATE TABLE catalog.connection_requirements (
    tenant_id        text NOT NULL CHECK (tenant_id <> ''),
    artifact_hash    text NOT NULL CHECK (artifact_hash <> ''),
    requirement_name text NOT NULL CHECK (requirement_name <> ''),
    requirement_json jsonb NOT NULL CHECK (jsonb_typeof(requirement_json) = 'object'),
    requirement_hash text NOT NULL CHECK (requirement_hash <> ''),
    created_at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, artifact_hash, requirement_name)
);
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

CREATE FUNCTION catalog.require_connection_artifact()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM catalog.flow_artifacts artifact
        WHERE artifact.tenant_id = NEW.tenant_id
          AND artifact.artifact_hash = NEW.artifact_hash
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = '23503',
            MESSAGE = 'connection-requirement-artifact-missing';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER connection_requirements_require_artifact
BEFORE INSERT ON catalog.connection_requirements
FOR EACH ROW EXECUTE FUNCTION catalog.require_connection_artifact();

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
    artifact_hash    text NOT NULL CHECK (artifact_hash <> ''),
    requirement_name text NOT NULL CHECK (requirement_name <> ''),
    environment      text NOT NULL CHECK (environment <> ''),
    instance_id      text NOT NULL CHECK (instance_id <> ''),
    binding_status   text NOT NULL DEFAULT 'active'
        CHECK (binding_status IN ('active', 'disabled')),
    validation_status text NOT NULL
        CHECK (validation_status IN ('valid', 'invalid')),
    validation_hash  text NOT NULL CHECK (validation_hash <> ''),
    created_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, catalog_id, catalog_version, artifact_hash, requirement_name),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.release_manifests (tenant_id, catalog_id, catalog_version),
    FOREIGN KEY (tenant_id, artifact_hash, requirement_name)
        REFERENCES catalog.connection_requirements
            (tenant_id, artifact_hash, requirement_name),
    FOREIGN KEY (tenant_id, environment, instance_id)
        REFERENCES catalog.connection_instances (tenant_id, environment, instance_id)
);
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

CREATE FUNCTION catalog.require_binding_release_environment()
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
    ) OR NOT EXISTS (
        SELECT 1
        FROM catalog.release_flows member
        JOIN catalog.flow_artifacts artifact
          ON artifact.tenant_id = member.tenant_id
         AND artifact.flow_id = member.flow_id
         AND artifact.flow_version = member.flow_version
        WHERE member.tenant_id = NEW.tenant_id
          AND member.catalog_id = NEW.catalog_id
          AND member.catalog_version = NEW.catalog_version
          AND artifact.artifact_hash = NEW.artifact_hash
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = '23514',
            MESSAGE = 'connection-binding-environment-mismatch';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER connection_bindings_match_release_environment
BEFORE INSERT ON catalog.connection_bindings
FOR EACH ROW EXECUTE FUNCTION catalog.require_binding_release_environment();

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

-- Draft execution is default-deny at one exact environment-owned connection
-- generation. A grant never follows the instance's active-generation pointer:
-- a successor generation needs its own row. The trusted development-admin
-- provisioning seeds the sole sandbox generation. Runtime and management may
-- inspect the relation for draft enforcement, but neither may mutate it.
-- BEGIN AUTHORING CONNECTION AUTHORITY MIGRATION (wamn-ftfc.11)
CREATE TABLE catalog.draft_safe_connection_grants (
    tenant_id    text NOT NULL CHECK (tenant_id <> ''),
    environment  text NOT NULL CHECK (environment <> ''),
    instance_id  text NOT NULL CHECK (instance_id <> ''),
    generation   bigint NOT NULL CHECK (generation > 0),
    reason       text NOT NULL CHECK (reason <> ''),
    granted_at   timestamptz NOT NULL DEFAULT now(),
    revoked_at   timestamptz,
    PRIMARY KEY (tenant_id, environment, instance_id, generation),
    FOREIGN KEY (tenant_id, environment, instance_id, generation)
        REFERENCES catalog.connection_generations
            (tenant_id, environment, instance_id, generation),
    CONSTRAINT draft_safe_connection_grants_revocation_time CHECK (
        revoked_at IS NULL OR revoked_at >= granted_at
    )
);
ALTER TABLE catalog.draft_safe_connection_grants ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.draft_safe_connection_grants FORCE ROW LEVEL SECURITY;
CREATE POLICY draft_safe_connection_grants_tenant
    ON catalog.draft_safe_connection_grants
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON catalog.draft_safe_connection_grants
    TO wamn_app, wamn_scenario_author;

CREATE FUNCTION catalog.guard_draft_safe_connection_grant_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF (NEW.tenant_id, NEW.environment, NEW.instance_id, NEW.generation)
       IS DISTINCT FROM
       (OLD.tenant_id, OLD.environment, OLD.instance_id, OLD.generation)
       OR NOT (
           -- Revoke: preserve the grant event and attach one revocation time.
           (OLD.revoked_at IS NULL
            AND NEW.revoked_at IS NOT NULL
            AND NEW.reason IS NOT DISTINCT FROM OLD.reason
            AND NEW.granted_at IS NOT DISTINCT FROM OLD.granted_at)
           OR
           -- Re-grant: clear the revocation and create a strictly later grant
           -- event, even when both operations occur inside one clock tick.
           (OLD.revoked_at IS NOT NULL
            AND NEW.revoked_at IS NULL
            AND NEW.granted_at > OLD.granted_at
            AND NEW.granted_at > OLD.revoked_at)
       ) THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'draft-safe-connection-grant-uncontrolled-update';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER draft_safe_connection_grants_controlled_update
BEFORE UPDATE ON catalog.draft_safe_connection_grants
FOR EACH ROW EXECUTE FUNCTION catalog.guard_draft_safe_connection_grant_update();
CREATE TRIGGER draft_safe_connection_grants_delete_immutable
BEFORE DELETE ON catalog.draft_safe_connection_grants
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
-- END AUTHORING CONNECTION AUTHORITY MIGRATION (wamn-ftfc.11)

-- BEGIN AUTHORING COMMAND AUDIT MIGRATION (wamn-ctc8.8)
-- ---------------------------------------------------------------------------
-- Authoring command ledger — WHO ran each authoring command and its exact
-- completed outcome. The management transport
-- (services/scenario-worker/src/management.rs) performs the mutation and
-- appends this completed row in one transaction, so neither commits without
-- the other. Two principals issuing the same command stay distinguishable
-- afterwards even though the command's own storage keeps no history
-- (catalog.flow_drafts is a destructive upsert).
--
-- CROSS-PLANE PRINCIPAL, DELIBERATELY NOT AN FK: principals live in the T1
-- system database (identity.principals) and this ledger lives in the project
-- database, so the attribution columns are denormalized text that stand alone —
-- the same shape, for the same reason, as wamn_run.operator_run_actions.
-- `principal_id` is the opaque T1 uuid as text; keeping the subject beside it
-- leaves the row readable after a subject is renamed upstream.
--
-- `effective_role` is the MANAGEMENT vocabulary, not identity storage's: role
-- slugs are opaque in identity.project_roles by design and gain meaning only at
-- the authorization boundary, which is what this CHECK pins.
--
-- `command_kind` carries the wire-contract spelling (crates/authoring/model
-- AuthoringCommandKind). Retry identity is exactly tenant + verified principal
-- + client command ID. The request hash binds that identity to canonical
-- contract bytes; the exact response envelope is retained for byte-identical
-- replay. A different principal therefore cannot read or replay this row.
--
-- APPEND-ONLY: insert-once, with UPDATE and DELETE both refused by the shared
-- immutability trigger. Audit evidence a careless writer can rewrite is not
-- evidence. Only the author role is granted anything; `wamn_app` (the guest
-- runtime credential) gets nothing at all — the ledger is management-plane state
-- and no flow may read or forge it.
-- ---------------------------------------------------------------------------
CREATE TABLE catalog.authoring_command_audit (
    tenant_id         text NOT NULL CHECK (tenant_id <> ''),
    audit_id          uuid NOT NULL DEFAULT gen_random_uuid(),
    command_id        text NOT NULL CHECK (command_id <> ''),
    command_kind      text NOT NULL,
    principal_id      text NOT NULL CHECK (principal_id <> ''),
    principal_kind    text NOT NULL,
    principal_subject text NOT NULL CHECK (principal_subject <> ''),
    effective_role    text NOT NULL,
    org               text NOT NULL CHECK (org <> ''),
    project           text NOT NULL CHECK (project <> ''),
    environment       text NOT NULL CHECK (environment <> ''),
    target_ref        text NOT NULL CHECK (target_ref <> ''),
    request_hash      text NOT NULL,
    outcome_bytes     bytea NOT NULL,
    -- ATTRIBUTION, NEVER AUTHORITY (wamn-ftfc.2). The client's own unverified
    -- claim about the working tree it read the content from. The platform
    -- clones no repository, runs no Git, and checks none of these values. They
    -- sit beside principal_id/principal_subject, which are the verified
    -- identity that actually authorized this command; no read path may
    -- substitute one for the other. NULL means the client claimed nothing.
    provenance_commit text,
    provenance_ref    text,
    provenance_dirty  boolean,
    -- Wall-clock audit time: two rows written in one transaction still order.
    recorded_at       timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, principal_id, command_id),
    CONSTRAINT authoring_command_audit_audit_id_key UNIQUE (tenant_id, audit_id),
    CONSTRAINT authoring_command_audit_request_hash_check
        CHECK (request_hash ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT authoring_command_audit_outcome_present
        CHECK (octet_length(outcome_bytes) > 0),
    CONSTRAINT authoring_command_audit_command_kind_check
        CHECK (command_kind IN ('save-flow-draft', 'validate', 'draft-run',
                                'test-set-run', 'publish')),
    CONSTRAINT authoring_command_audit_principal_kind_check
        CHECK (principal_kind IN ('human', 'service')),
    CONSTRAINT authoring_command_audit_effective_role_check
        CHECK (effective_role IN ('project-author', 'project-admin')),
    CONSTRAINT authoring_command_audit_provenance_check
        CHECK ((provenance_commit IS NULL) = (provenance_dirty IS NULL)
               AND (provenance_commit IS NULL OR provenance_commit <> '')
               AND (provenance_ref IS NULL OR provenance_ref <> '')
               AND (provenance_commit IS NOT NULL OR provenance_ref IS NULL))
);
ALTER TABLE catalog.authoring_command_audit ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.authoring_command_audit FORCE ROW LEVEL SECURITY;
CREATE POLICY authoring_command_audit_tenant ON catalog.authoring_command_audit
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT ON catalog.authoring_command_audit TO wamn_scenario_author;
CREATE TRIGGER authoring_command_audit_immutable
BEFORE UPDATE OR DELETE ON catalog.authoring_command_audit
FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
CREATE INDEX authoring_command_audit_recorded
    ON catalog.authoring_command_audit (tenant_id, recorded_at);
-- END AUTHORING COMMAND AUDIT MIGRATION (wamn-ctc8.8)

-- BEGIN AUTHORING DRAFT DEFINITION MIGRATION (wamn-ftfc.2)
-- Additive upgrade for catalogs provisioned before the mutable draft was
-- stored as exact text. `jsonb` reparsed and normalized what a client sent, so
-- a saved revision could not be handed back byte for byte and a half-finished
-- edit could not be saved at all — both of which the authoring contract
-- promises. `validate` parses this text at its own stage and owns the refusal.
--
-- NO BACKFILL, DELIBERATELY. A pre-migration row's exact bytes were destroyed
-- by the `jsonb` cast and cannot be recovered; writing the normalized document
-- into a column that promises exactness would manufacture that promise. Those
-- rows keep `definition NULL`, readers fall back to `graph_json`, and the next
-- save makes the row exact. This also keeps the upgrade write-free, so it adds
-- no state-ownership writer to deploy/sql/catalog-schema.sql.
ALTER TABLE catalog.flow_drafts
    ADD COLUMN IF NOT EXISTS definition text;
ALTER TABLE catalog.flow_drafts
    ALTER COLUMN graph_json DROP NOT NULL;
ALTER TABLE catalog.flow_drafts
    DROP CONSTRAINT IF EXISTS flow_drafts_content_present;
ALTER TABLE catalog.flow_drafts
    ADD CONSTRAINT flow_drafts_content_present
    CHECK (definition IS NOT NULL OR graph_json IS NOT NULL);
-- END AUTHORING DRAFT DEFINITION MIGRATION (wamn-ftfc.2)

-- BEGIN AUTHORING COMMAND PROVENANCE MIGRATION (wamn-ftfc.2)
-- Additive upgrade for ledgers provisioned before optional source attribution
-- existed. Existing rows deliberately remain NULL: a command recorded before
-- this migration carried no claim, and inventing one would be fabricated
-- evidence. See the column comments above for why this is never authority.
ALTER TABLE catalog.authoring_command_audit
    ADD COLUMN IF NOT EXISTS provenance_commit text;
ALTER TABLE catalog.authoring_command_audit
    ADD COLUMN IF NOT EXISTS provenance_ref text;
ALTER TABLE catalog.authoring_command_audit
    ADD COLUMN IF NOT EXISTS provenance_dirty boolean;
ALTER TABLE catalog.authoring_command_audit
    DROP CONSTRAINT IF EXISTS authoring_command_audit_provenance_check;
ALTER TABLE catalog.authoring_command_audit
    ADD CONSTRAINT authoring_command_audit_provenance_check
    CHECK ((provenance_commit IS NULL) = (provenance_dirty IS NULL)
           AND (provenance_commit IS NULL OR provenance_commit <> '')
           AND (provenance_ref IS NULL OR provenance_ref <> '')
           AND (provenance_commit IS NOT NULL OR provenance_ref IS NULL));
-- END AUTHORING COMMAND PROVENANCE MIGRATION (wamn-ftfc.2)

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
