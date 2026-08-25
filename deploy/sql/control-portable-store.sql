-- Portable authoring, release, and test storage for the T1
-- control database (wamn-0h0g.9.9). Apply after system-schema.sql as
-- wamn_system. This artifact is deliberately dormant: it grants no production
-- role access and installs no project/runtime compatibility path.
--
-- The qualified names are intentionally unchanged from the project copies.
-- During the one-cutover train both databases can therefore carry catalog.*
-- and wamn_run.authoring_test_*; database residency, not a renamed schema,
-- distinguishes them.
--
-- `control-portable-retained-shape-drift` and the deployment-attestation
-- constraint fingerprint below are apply-time digests: they must be regenerated
-- whenever a retained relation's shape moves; the owning schema change
-- regenerates both.

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

CREATE TABLE IF NOT EXISTS catalog.catalogs (
    tenant_id      text NOT NULL CHECK (tenant_id <> ''),
    catalog_id     text NOT NULL,
    version        int NOT NULL,
    environment    text NOT NULL DEFAULT 'dev',
    schema_version text NOT NULL,
    name           text,
    state          text NOT NULL DEFAULT 'draft',
    base_version   int,
    document       jsonb,
    PRIMARY KEY (tenant_id, catalog_id, version),
    CONSTRAINT catalogs_state_check
        CHECK (state IN ('draft', 'staged', 'applied', 'superseded'))
);
CREATE UNIQUE INDEX IF NOT EXISTS catalogs_one_applied_per_env
    ON catalog.catalogs (tenant_id, catalog_id, environment)
    WHERE state = 'applied';

CREATE TABLE IF NOT EXISTS catalog.flow_artifacts (
    tenant_id                text NOT NULL CHECK (tenant_id <> ''),
    flow_id                  text NOT NULL,
    flow_version             int NOT NULL CHECK (flow_version > 0),
    schema_version           text NOT NULL,
    graph_json               jsonb NOT NULL,
    graph_hash               text NOT NULL,
    artifact_hash            text NOT NULL,
    verified_author_principal text
        CHECK (verified_author_principal IS NULL OR verified_author_principal <> ''),
    created_at               timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, flow_id, flow_version)
);

CREATE TABLE IF NOT EXISTS catalog.releases (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    catalog_version int NOT NULL,
    verified_publisher_principal text
        CHECK (verified_publisher_principal IS NULL OR verified_publisher_principal <> ''),
    PRIMARY KEY (tenant_id, catalog_id, catalog_version),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.catalogs (tenant_id, catalog_id, version)
);

CREATE TABLE IF NOT EXISTS catalog.release_flows (
    tenant_id            text NOT NULL CHECK (tenant_id <> ''),
    catalog_id           text NOT NULL,
    catalog_version      int NOT NULL,
    flow_id              text NOT NULL,
    flow_version         int NOT NULL,
    PRIMARY KEY (tenant_id, catalog_id, catalog_version, flow_id),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.releases (tenant_id, catalog_id, catalog_version),
    FOREIGN KEY (tenant_id, flow_id, flow_version)
        REFERENCES catalog.flow_artifacts (tenant_id, flow_id, flow_version)
);

CREATE TABLE IF NOT EXISTS catalog.catalog_heads (
    tenant_id              text NOT NULL CHECK (tenant_id <> ''),
    catalog_id             text NOT NULL,
    environment            text NOT NULL,
    applied_catalog_version int NOT NULL,
    updated_at             timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, catalog_id, environment),
    FOREIGN KEY (tenant_id, catalog_id, applied_catalog_version)
        REFERENCES catalog.catalogs (tenant_id, catalog_id, version)
);

CREATE TABLE IF NOT EXISTS catalog.component_library (
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
-- wamn-0h0g.21.9: the derived effect projection, additive onto a store created
-- before it existed. Rows admitted earlier read as '[]' — "pure" — which their
-- validator never derived. wamn-0h0g.21.10 refuses that claim instead of
-- rewriting it, which an immutable relation could not accept anyway: every
-- reader re-derives the projection from the row's own audited imports and
-- refuses a row the two do not agree on. A pre-migration component importing
-- nothing that leaves the host keeps its '[]' — now derived rather than
-- asserted; one that does is unpublishable until re-admitted through the
-- validator. See wamn_catalog::verify_stored_effect_projection.
ALTER TABLE catalog.component_library
    ADD COLUMN IF NOT EXISTS effects jsonb NOT NULL DEFAULT '[]'::jsonb
        CHECK (jsonb_typeof(effects) = 'array');
ALTER TABLE catalog.component_library
    ALTER COLUMN effects DROP DEFAULT;

CREATE TABLE IF NOT EXISTS catalog.flow_drafts (
    tenant_id  text NOT NULL CHECK (tenant_id <> ''),
    draft_id   text NOT NULL CHECK (draft_id <> ''),
    flow_id    text NOT NULL CHECK (flow_id <> ''),
    revision   bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    definition text,
    graph_json jsonb CHECK (jsonb_typeof(graph_json) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now(),
    edited_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, draft_id),
    CONSTRAINT flow_drafts_content_present
        CHECK (definition IS NOT NULL OR graph_json IS NOT NULL)
);

CREATE OR REPLACE FUNCTION catalog.guard_flow_draft_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF (NEW.tenant_id, NEW.draft_id, NEW.flow_id, NEW.created_at)
       IS DISTINCT FROM
       (OLD.tenant_id, OLD.draft_id, OLD.flow_id, OLD.created_at)
       OR NEW.revision <> OLD.revision + 1
       OR NEW.edited_at <= OLD.edited_at THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'flow-draft-uncontrolled-update';
    END IF;
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION catalog.guard_flow_draft_update() FROM PUBLIC;

-- No validated-draft relation (wamn-pm7k): the draft concept died with the
-- pivot. The wiring document IS the validated artifact and its hash IS the
-- identity, so `validated_draft_id` below is read as the wiring hash and needs
-- no separate lineage row to resolve it.

CREATE TABLE IF NOT EXISTS catalog.release_exposure_manifests (
    tenant_id        text NOT NULL CHECK (tenant_id <> ''),
    catalog_id       text NOT NULL,
    catalog_version  int NOT NULL,
    definitions_json jsonb NOT NULL CHECK (jsonb_typeof(definitions_json) = 'object'),
    PRIMARY KEY (tenant_id, catalog_id, catalog_version),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.releases (tenant_id, catalog_id, catalog_version)
);

CREATE TABLE IF NOT EXISTS catalog.release_sources (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    catalog_version int NOT NULL,
    source_id       text NOT NULL,
    source_kind     text NOT NULL CHECK (source_kind IN ('auth', 'caller-policy', 'schedule')),
    definition_json jsonb NOT NULL CHECK (jsonb_typeof(definition_json) = 'object'),
    source_hash     text NOT NULL,
    PRIMARY KEY (tenant_id, catalog_id, catalog_version, source_id),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.release_exposure_manifests
            (tenant_id, catalog_id, catalog_version)
);

CREATE TABLE IF NOT EXISTS catalog.release_attachments (
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
        REFERENCES catalog.release_sources
            (tenant_id, catalog_id, catalog_version, source_id),
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

CREATE TABLE IF NOT EXISTS catalog.connection_requirements (
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
-- Existing rows retain their legacy coordinates. No component provenance is
-- synthesized from an artifact hash during this additive transition.
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

-- The referenced connection generation is project-local after the plane split.
-- Its coordinates remain plain identity here; a cross-database FK is forbidden.
CREATE TABLE IF NOT EXISTS catalog.draft_safe_connection_grants (
    tenant_id   text NOT NULL CHECK (tenant_id <> ''),
    environment text NOT NULL CHECK (environment <> ''),
    instance_id text NOT NULL CHECK (instance_id <> ''),
    generation  bigint NOT NULL CHECK (generation > 0),
    reason      text NOT NULL CHECK (reason <> ''),
    granted_at  timestamptz NOT NULL DEFAULT now(),
    revoked_at  timestamptz,
    PRIMARY KEY (tenant_id, environment, instance_id, generation),
    CONSTRAINT draft_safe_connection_grants_revocation_time
        CHECK (revoked_at IS NULL OR revoked_at >= granted_at)
);

CREATE TABLE IF NOT EXISTS catalog.authoring_command_audit (
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
    provenance_commit text,
    provenance_ref    text,
    provenance_dirty  boolean,
    recorded_at       timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, principal_id, command_id),
    CONSTRAINT authoring_command_audit_audit_id_key UNIQUE (tenant_id, audit_id),
    CONSTRAINT authoring_command_audit_request_hash_check
        CHECK (request_hash ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT authoring_command_audit_outcome_present
        CHECK (octet_length(outcome_bytes) > 0),
    CONSTRAINT authoring_command_audit_command_kind_check
        CHECK (command_kind IN ('save-draft', 'validate', 'draft-run',
                                'test-set-run', 'publish')),
    CONSTRAINT authoring_command_audit_principal_kind_check
        CHECK (principal_kind IN ('human', 'service')),
    CONSTRAINT authoring_command_audit_effective_role_check
        CHECK (effective_role IN ('project-author', 'project-admin')),
    CONSTRAINT authoring_command_audit_provenance_check CHECK (
        (provenance_commit IS NULL) = (provenance_dirty IS NULL)
        AND (provenance_commit IS NULL OR provenance_commit <> '')
        AND (provenance_ref IS NULL OR provenance_ref <> '')
        AND (provenance_commit IS NOT NULL OR provenance_ref IS NULL)
    )
);
CREATE INDEX IF NOT EXISTS authoring_command_audit_recorded
    ON catalog.authoring_command_audit (tenant_id, recorded_at);
-- wamn-0h0g.26.18: the command ledger's vocabulary is the contract's, so the
-- `save-flow-draft` spelling is retired to `save-draft` here as well. CREATE
-- TABLE IF NOT EXISTS cannot reach a store provisioned before the rename, and
-- `control-portable-retained-shape-drift` hashes this constraint's definition,
-- so the converging ALTER is what keeps an existing store applying. A store
-- holding a legacy `save-flow-draft` audit row refuses here by name rather than
-- carrying two vocabularies for one command.
ALTER TABLE catalog.authoring_command_audit
    DROP CONSTRAINT IF EXISTS authoring_command_audit_command_kind_check,
    ADD CONSTRAINT authoring_command_audit_command_kind_check
        CHECK (command_kind IN ('save-draft', 'validate', 'draft-run',
                                'test-set-run', 'publish'));

CREATE TABLE IF NOT EXISTS wamn_run.authoring_test_run_reservations (
    tenant_id          text NOT NULL CHECK (tenant_id <> ''),
    report_id          text NOT NULL CHECK (report_id <> ''),
    command_hash       text NOT NULL CHECK (command_hash ~ '^sha256:[0-9a-f]{64}$'),
    validated_draft_id text NOT NULL CHECK (validated_draft_id <> ''),
    catalog_id         text NOT NULL CHECK (catalog_id <> ''),
    catalog_version    int NOT NULL CHECK (catalog_version > 0),
    case_count         int NOT NULL CHECK (case_count BETWEEN 1 AND 256),
    state             text NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'finalized')),
    created_at        timestamptz NOT NULL DEFAULT clock_timestamp(),
    whole_deadline_at timestamptz NOT NULL,
    finalized_at      timestamptz,
    PRIMARY KEY (tenant_id, report_id),
    UNIQUE (tenant_id, report_id, catalog_id, catalog_version, validated_draft_id),
    CHECK (whole_deadline_at > created_at),
    CHECK (
        (state = 'pending' AND finalized_at IS NULL)
        OR (state = 'finalized' AND finalized_at IS NOT NULL
            AND finalized_at >= created_at)
    )
);

CREATE TABLE IF NOT EXISTS wamn_run.authoring_test_case_runs (
    tenant_id          text NOT NULL CHECK (tenant_id <> ''),
    report_id          text NOT NULL CHECK (report_id <> ''),
    ordinal            int NOT NULL CHECK (ordinal BETWEEN 0 AND 255),
    case_id            text NOT NULL CHECK (case_id <> ''),
    run_id             text NOT NULL CHECK (run_id <> ''),
    catalog_id         text NOT NULL CHECK (catalog_id <> ''),
    catalog_version    int NOT NULL CHECK (catalog_version > 0),
    validated_draft_id text NOT NULL CHECK (validated_draft_id <> ''),
    state              text NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'finalized')),
    passed             boolean,
    failure_kind       text CHECK (
        failure_kind IN ('assertion-failed', 'deadline-exhausted',
                         'effect-uncertain')
    ),
    summary             jsonb CHECK (summary IS NULL OR jsonb_typeof(summary) = 'object'),
    case_deadline_at    timestamptz NOT NULL,
    created_at          timestamptz NOT NULL DEFAULT clock_timestamp(),
    finalized_at        timestamptz,
    PRIMARY KEY (tenant_id, report_id, ordinal),
    UNIQUE (tenant_id, report_id, case_id),
    UNIQUE (tenant_id, run_id),
    FOREIGN KEY (tenant_id, report_id, catalog_id, catalog_version, validated_draft_id)
        REFERENCES wamn_run.authoring_test_run_reservations
            (tenant_id, report_id, catalog_id, catalog_version, validated_draft_id),
    CHECK (case_deadline_at > created_at),
    CHECK (
        (state = 'pending' AND passed IS NULL AND failure_kind IS NULL
         AND summary IS NULL AND finalized_at IS NULL)
        OR (state = 'finalized' AND passed IS NOT NULL AND summary IS NOT NULL
            AND finalized_at IS NOT NULL AND finalized_at >= created_at
            AND ((passed AND failure_kind IS NULL)
                 OR (NOT passed AND failure_kind IS NOT NULL)))
    )
);

CREATE TABLE IF NOT EXISTS wamn_run.authoring_test_reports (
    tenant_id           text NOT NULL CHECK (tenant_id <> ''),
    report_id           text NOT NULL CHECK (report_id <> ''),
    validated_draft_id  text NOT NULL CHECK (validated_draft_id <> ''),
    catalog_id          text NOT NULL CHECK (catalog_id <> ''),
    catalog_version     int NOT NULL CHECK (catalog_version > 0),
    passed              boolean NOT NULL,
    summary             jsonb NOT NULL CHECK (jsonb_typeof(summary) = 'object'),
    finalized_at        timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, report_id),
    FOREIGN KEY (tenant_id, report_id)
        REFERENCES wamn_run.authoring_test_run_reservations (tenant_id, report_id)
);

CREATE TABLE IF NOT EXISTS catalog.deployment_attestations (
    tenant_id               text NOT NULL CHECK (tenant_id <> ''),
    catalog_id              text NOT NULL CHECK (catalog_id <> ''),
    catalog_version         int NOT NULL CHECK (catalog_version > 0),
    org_id                  text NOT NULL CHECK (org_id <> ''),
    project_id              text NOT NULL CHECK (project_id <> ''),
    environment             text NOT NULL CHECK (environment <> ''),
    deployed_manifest_hash  text NOT NULL CHECK (deployed_manifest_hash ~ '^sha256:[0-9a-f]{64}$'),
    attested_at             timestamptz NOT NULL,
    CONSTRAINT deployment_attestations_coordinate UNIQUE (
        tenant_id, catalog_id, catalog_version, org_id, project_id, environment
    ),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.releases (tenant_id, catalog_id, catalog_version)
);

CREATE OR REPLACE FUNCTION catalog.register_deployment_attestation(
    p_tenant_id text,
    p_catalog_id text,
    p_catalog_version int,
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
    existing_attested_at timestamptz;
BEGIN
    INSERT INTO catalog.deployment_attestations (
        tenant_id, catalog_id, catalog_version, org_id, project_id,
        environment, deployed_manifest_hash, attested_at
    ) VALUES (
        p_tenant_id, p_catalog_id, p_catalog_version, p_org_id, p_project_id,
        p_environment, p_deployed_manifest_hash, p_attested_at
    )
    ON CONFLICT (
        tenant_id, catalog_id, catalog_version, org_id, project_id, environment
    ) DO NOTHING
    RETURNING attested_at INTO existing_attested_at;

    IF existing_attested_at IS NOT NULL THEN
        RETURN existing_attested_at;
    END IF;

    SELECT attested_at INTO existing_attested_at
    FROM catalog.deployment_attestations
    WHERE tenant_id = p_tenant_id
      AND catalog_id = p_catalog_id
      AND catalog_version = p_catalog_version
      AND org_id = p_org_id
      AND project_id = p_project_id
      AND environment = p_environment
      AND deployed_manifest_hash = p_deployed_manifest_hash
      AND attested_at = p_attested_at;

    IF existing_attested_at IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '23505',
            MESSAGE = 'deployment-attestation-content-conflict';
    END IF;
    RETURN existing_attested_at;
END
$$;
REVOKE ALL ON FUNCTION catalog.register_deployment_attestation(
    text, text, int, text, text, text, text, timestamptz
) FROM PUBLIC;

-- wamn-0h0g.26.16: flow-shaped release TEST EVIDENCE is retired. The row named
-- a release member by `flow_id` under an identity that no longer exists, and no
-- caller in the workspace ever executed its registrar. The rows are deliberately
-- discarded without archive or conversion, exactly like the bundle bytes below.
--
-- The routine goes first, and by NAME over every overload rather than by pinned
-- signature: a superseded signature that outlived its pin would otherwise
-- survive as an owner-only entry point onto a dropped relation. The table then
-- goes BEFORE the execution-bundle block, whose RESTRICT drop would otherwise
-- trip on the evidence foreign key it used to carry.
DO $retire_release_flow_test_evidence$
DECLARE
    retired_routine text;
BEGIN
    FOR retired_routine IN
        SELECT routine.oid::regprocedure::text
          FROM pg_proc AS routine
          JOIN pg_namespace AS namespace ON namespace.oid = routine.pronamespace
         WHERE namespace.nspname = 'catalog'
           AND routine.proname = 'register_release_flow_test_evidence'
    LOOP
        EXECUTE format('DROP FUNCTION %s', retired_routine);
    END LOOP;
    IF to_regclass('catalog.release_flow_test_evidence') IS NOT NULL THEN
        LOCK TABLE catalog.release_flow_test_evidence IN ACCESS EXCLUSIVE MODE;
        DROP TABLE catalog.release_flow_test_evidence RESTRICT;
    END IF;
END
$retire_release_flow_test_evidence$;

-- Persisted bundle bytes are deliberately discarded without archive or
-- conversion. Drop every direct carrier first so the final RESTRICT drop is a
-- dependency tripwire, not an instruction to amputate an un-inventoried object.
DO $retire_execution_bundles$
BEGIN
    LOCK TABLE catalog.release_flows IN ACCESS EXCLUSIVE MODE;
    ALTER TABLE catalog.release_flows
        DROP COLUMN IF EXISTS execution_bundle_hash RESTRICT;
    IF to_regclass('catalog.execution_bundles') IS NOT NULL THEN
        LOCK TABLE catalog.execution_bundles IN ACCESS EXCLUSIVE MODE;
        DROP TABLE catalog.execution_bundles RESTRICT;
    END IF;
END
$retire_execution_bundles$;

-- Tenant isolation is structural even while the dormant tables are owner-only.
DO $policies$
DECLARE
    relation_name text;
    policy_name text;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'catalog.catalogs', 'catalog.flow_artifacts',
        'catalog.releases',
        'catalog.release_flows', 'catalog.catalog_heads',
        'catalog.flow_drafts',
        'catalog.release_exposure_manifests', 'catalog.release_sources',
        'catalog.release_attachments', 'catalog.component_library',
        'catalog.connection_requirements',
        'catalog.draft_safe_connection_grants', 'catalog.authoring_command_audit',
        'catalog.deployment_attestations',
        'wamn_run.authoring_test_run_reservations',
        'wamn_run.authoring_test_case_runs', 'wamn_run.authoring_test_reports'
    ]
    LOOP
        EXECUTE format('ALTER TABLE %s ENABLE ROW LEVEL SECURITY', relation_name);
        EXECUTE format('ALTER TABLE %s FORCE ROW LEVEL SECURITY', relation_name);
        policy_name := split_part(relation_name, '.', 2) || '_tenant';
        EXECUTE format(
            'DROP POLICY IF EXISTS %I ON %s',
            policy_name,
            relation_name
        );
        EXECUTE format(
            'CREATE POLICY %I ON %s USING (tenant_id = NULLIF(current_setting(''app.tenant'', true), '''')) WITH CHECK (tenant_id = NULLIF(current_setting(''app.tenant'', true), ''''))',
            policy_name,
            relation_name
        );
    END LOOP;
END
$policies$;

-- Reconcile immutable guards without duplicating them on replay.
DO $triggers$
DECLARE
    relation_name text;
    trigger_name text;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'catalog.flow_artifacts',
        'catalog.releases', 'catalog.release_flows',
        'catalog.release_exposure_manifests',
        'catalog.release_sources', 'catalog.release_attachments',
        'catalog.component_library',
        'catalog.connection_requirements', 'catalog.authoring_command_audit',
        'catalog.deployment_attestations',
        'wamn_run.authoring_test_reports'
    ]
    LOOP
        trigger_name := split_part(relation_name, '.', 2) || '_immutable';
        IF NOT EXISTS (
            SELECT 1 FROM pg_trigger
            WHERE tgrelid = relation_name::regclass
              AND tgname = trigger_name
              AND NOT tgisinternal
        ) THEN
            EXECUTE format(
                'CREATE TRIGGER %I BEFORE UPDATE OR DELETE ON %s FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change()',
                trigger_name,
                relation_name
            );
        END IF;
    END LOOP;

    IF NOT EXISTS (
        SELECT 1 FROM pg_trigger
        WHERE tgrelid = 'catalog.flow_drafts'::regclass
          AND tgname = 'flow_drafts_controlled_update'
          AND NOT tgisinternal
    ) THEN
        CREATE TRIGGER flow_drafts_controlled_update
        BEFORE UPDATE ON catalog.flow_drafts
        FOR EACH ROW EXECUTE FUNCTION catalog.guard_flow_draft_update();
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_trigger
        WHERE tgrelid = 'catalog.flow_drafts'::regclass
          AND tgname = 'flow_drafts_delete_immutable'
          AND NOT tgisinternal
    ) THEN
        CREATE TRIGGER flow_drafts_delete_immutable
        BEFORE DELETE ON catalog.flow_drafts
        FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
    END IF;
END
$triggers$;

-- Retirement is catalog-driven: the retired object is itself the record that a
-- database predates this artifact, so a converged replay finds nothing to do and
-- no apply ledger is needed. What can converge in place is fixed by what this
-- artifact asserts about itself below. The evidence and attestation column
-- asserts compare name, type and nullability in attnum ORDER but never the
-- attnum, so a DROP COLUMN reaches the asserted shape;
-- `control-portable-retained-shape-drift` hashes a.attnum itself, so on a
-- RETAINED relation no ALTER can — a dropped column keeps its slot and an added
-- one lands past the tail. Those refuse by name rather than half-migrate, and
-- they cannot be traded for a data-preserving rebuild here: under FORCE ROW
-- LEVEL SECURITY with no `app.tenant`, the applying owner reads zero rows from
-- every retained relation, so an emptiness guard or an `INSERT ... SELECT` copy
-- would silently treat a populated table as empty (wamn-0h0g.15.91).
DROP FUNCTION IF EXISTS catalog.register_deployment_attestation(
    text, text, int, text, text, text, text, jsonb, timestamptz
);

DO $retire$
BEGIN
    -- wamn-pm7k: the draft concept died with the pivot. The wiring document IS
    -- the validated artifact and its hash IS the identity, so the relation has
    -- no writer and its rows name nothing — they are deliberately discarded
    -- without archive, exactly like the bundle bytes above. The one dependency
    -- the RESTRICT drop used to refuse on — a release-evidence foreign key that
    -- resolved an identity through this relation — left with the evidence table
    -- itself (wamn-0h0g.26.16), which is dropped above.
    IF to_regclass('catalog.validated_flow_drafts') IS NOT NULL THEN
        LOCK TABLE catalog.validated_flow_drafts IN ACCESS EXCLUSIVE MODE;
        DROP TABLE catalog.validated_flow_drafts RESTRICT;
    END IF;

    -- wamn-0h0g.15.27 retired the test-set store, leaving both RETAINED record
    -- tables with a NOT NULL foreign-key column that has no default and
    -- therefore refuses every reservation and report INSERT.
    IF EXISTS (
        SELECT 1 FROM pg_attribute
        WHERE attrelid = to_regclass('wamn_run.authoring_test_run_reservations')
          AND attname = 'test_set_hash' AND NOT attisdropped
    ) OR EXISTS (
        SELECT 1 FROM pg_attribute
        WHERE attrelid = to_regclass('wamn_run.authoring_test_reports')
          AND attname = 'test_set_hash' AND NOT attisdropped
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'control-portable-retired-test-set-lineage-requires-reprovision';
    END IF;

    -- wamn-0h0g.15.159 dropped the sealed member snapshot from the release
    -- identity row; membership is row-per-member in catalog.release_flows. This
    -- is a RETAINED relation, so DROP COLUMN cannot reach the asserted shape:
    -- the dropped slot keeps its attnum and verified_publisher_principal never
    -- moves to 4, which the retained-shape digest hashes.
    IF EXISTS (
        SELECT 1 FROM pg_attribute
        WHERE attrelid = to_regclass('catalog.releases')
          AND attname = 'members_json' AND NOT attisdropped
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'control-portable-retired-release-members-requires-reprovision';
    END IF;

    -- wamn-0h0g.7.3 ratified the audit retry ledger as two NOT NULL columns
    -- mid-record, which an ALTER can only append past the tail.
    IF to_regclass('catalog.authoring_command_audit') IS NOT NULL AND (
        SELECT count(*) FROM pg_attribute
        WHERE attrelid = to_regclass('catalog.authoring_command_audit')
          AND attname IN ('request_hash', 'outcome_bytes') AND NOT attisdropped
    ) <> 2 THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'control-portable-retired-audit-ledger-requires-reprovision';
    END IF;

    -- Ruling 5 (wamn-0h0g.15.8): the deployed map is derivable from the digest
    -- it sat next to, and its superseded overload is dropped above.
    IF EXISTS (
        SELECT 1 FROM pg_attribute
        WHERE attrelid = to_regclass('catalog.deployment_attestations')
          AND attname = 'deployed_resolution_map' AND NOT attisdropped
    ) THEN
        ALTER TABLE catalog.deployment_attestations
            DROP COLUMN deployed_resolution_map;
    END IF;

    -- wamn-0h0g.15.27 also narrowed the case outcome domain. ADD CONSTRAINT
    -- validates the heap directly instead of through the policy, so a surviving
    -- retired outcome refuses here without a row guard RLS would blind.
    IF EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = to_regclass('wamn_run.authoring_test_case_runs')
          AND conname = 'authoring_test_case_runs_failure_kind_check'
          AND pg_get_constraintdef(oid, true) LIKE '%resolution-map-mismatch%'
    ) THEN
        ALTER TABLE wamn_run.authoring_test_case_runs
            DROP CONSTRAINT authoring_test_case_runs_failure_kind_check,
            ADD CONSTRAINT authoring_test_case_runs_failure_kind_check
            CHECK (failure_kind IN ('assertion-failed', 'deadline-exhausted',
                                    'effect-uncertain'));
    END IF;
END
$retire$;

-- Replay is a reconcile, not a best-effort `IF NOT EXISTS`: refuse a relation
-- inventory or either newly ratified record shape that differs from this
-- artifact. Focused Rust drift guards pin retained-copy columns to the project
-- source while that source remains authoritative during the cutover train.
DO $drift$
DECLARE
    catalog_tables text[];
    run_tables text[];
    attestation_columns text[];
    attestation_constraints_fingerprint text;
    retained_fingerprint text;
BEGIN
    SELECT array_agg(tablename ORDER BY tablename) INTO catalog_tables
    FROM pg_tables WHERE schemaname = 'catalog';
    IF catalog_tables IS DISTINCT FROM ARRAY[
        'authoring_command_audit', 'catalog_heads', 'catalogs',
        'component_library', 'connection_requirements', 'deployment_attestations',
        'draft_safe_connection_grants', 'flow_artifacts',
        'flow_drafts', 'release_attachments', 'release_exposure_manifests',
        'release_flows', 'release_sources',
        'releases'
    ]::text[] THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'control-portable-catalog-inventory-drift';
    END IF;

    SELECT array_agg(tablename ORDER BY tablename) INTO run_tables
    FROM pg_tables WHERE schemaname = 'wamn_run';
    IF run_tables IS DISTINCT FROM ARRAY[
        'authoring_test_case_runs', 'authoring_test_reports',
        'authoring_test_run_reservations'
    ]::text[] THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'control-portable-run-inventory-drift';
    END IF;

    SELECT array_agg(
        a.attname || ':' || pg_catalog.format_type(a.atttypid, a.atttypmod)
        || ':' || a.attnotnull::text ORDER BY a.attnum
    ) INTO attestation_columns
    FROM pg_attribute a
    WHERE a.attrelid = 'catalog.deployment_attestations'::regclass
      AND a.attnum > 0 AND NOT a.attisdropped;
    IF attestation_columns IS DISTINCT FROM ARRAY[
        'tenant_id:text:true', 'catalog_id:text:true',
        'catalog_version:integer:true', 'org_id:text:true',
        'project_id:text:true', 'environment:text:true',
        'deployed_manifest_hash:text:true',
        'attested_at:timestamp with time zone:true'
    ]::text[] THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'deployment-attestation-shape-drift';
    END IF;

    SELECT encode(sha256(convert_to(string_agg(
        con.contype::text || ':' || pg_get_constraintdef(con.oid, true),
        E'\n' ORDER BY (con.contype::text || ':'
        || pg_get_constraintdef(con.oid, true)) COLLATE "C"
    ), 'UTF8')), 'hex')
    INTO attestation_constraints_fingerprint
    FROM pg_constraint con
    WHERE con.conrelid = 'catalog.deployment_attestations'::regclass
      AND con.contype <> 'n';
    IF attestation_constraints_fingerprint <>
       'abae958ad104c5875743d1e39772eb576516aa5d2f175b2d684479a3f4b98415'
    THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'deployment-attestation-constraint-drift';
    END IF;

    WITH retained_relations(relation) AS (
        VALUES
            ('catalog.catalogs'), ('catalog.flow_artifacts'),
            ('catalog.releases'),
            ('catalog.release_flows'), ('catalog.catalog_heads'),
            ('catalog.component_library'),
            ('catalog.flow_drafts'),
            ('catalog.release_exposure_manifests'), ('catalog.release_sources'),
            ('catalog.release_attachments'), ('catalog.connection_requirements'),
            ('catalog.draft_safe_connection_grants'),
            ('catalog.authoring_command_audit'),
            ('wamn_run.authoring_test_run_reservations'),
            ('wamn_run.authoring_test_case_runs'),
            ('wamn_run.authoring_test_reports')
    ), facts AS (
        SELECT r.relation,
               'column:' || a.attnum || ':' || a.attname || ':'
               || pg_catalog.format_type(a.atttypid, a.atttypmod) || ':'
               || a.attnotnull || ':'
               || COALESCE(pg_get_expr(d.adbin, d.adrelid, true), '-') AS fact
        FROM retained_relations r
        JOIN pg_class c ON c.oid = r.relation::regclass
        JOIN pg_attribute a ON a.attrelid = c.oid
          AND a.attnum > 0 AND NOT a.attisdropped
        LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
        UNION ALL
        SELECT r.relation,
               'constraint:' || con.contype::text || ':'
               || pg_get_constraintdef(con.oid, true)
        FROM retained_relations r
        JOIN pg_class c ON c.oid = r.relation::regclass
        JOIN pg_constraint con ON con.conrelid = c.oid
        WHERE NOT (
            r.relation = 'catalog.draft_safe_connection_grants'
            AND con.contype = 'f'
        )
    )
    SELECT encode(sha256(convert_to(string_agg(
        relation || '|' || fact,
        E'\n' ORDER BY relation COLLATE "C", fact COLLATE "C"
    ), 'UTF8')), 'hex')
    INTO retained_fingerprint
    FROM facts;
    IF retained_fingerprint <>
       '1475daebde6d32ee0256ac90b5cf40cda67711f8bd294b434e7df40981c8c142'
    THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'control-portable-retained-shape-drift';
    END IF;
END
$drift$;

-- ---------------------------------------------------------------------------
-- Author authority (wamn-0h0g.8.18). This store stops being dormant for exactly
-- ONE principal: `wamn_control_author`, the stable NOLOGIN ACL role the
-- management service reaches through a scoped A/B LOGIN generation. ctl creates
-- and hardens the role and its generations
-- (crates/control/provision/src/sql.rs); this artifact owns only what the role
-- may touch once it exists, so the GRANTs below are unconditional exactly like
-- run-state.sql's grants to `wamn_scenario_author`. Applying this file therefore
-- requires the NOLOGIN role to already exist.
--
-- Deliberately placed AFTER the retirement and drift blocks: a store whose shape
-- has drifted refuses before any authority is granted on it.
--
-- Author, publisher/deployer, artifact reader, and effect writer stay four
-- separate principals. Nothing here grants deployment-attestation publication
-- (`catalog.deployment_attestations` and its register_* routine stay
-- owner-only), project run or binding authority, artifact-reader authority,
-- effect-writer authority, or UPDATE/DELETE over any immutable fact.
-- `wamn_scenario_author` is never granted anything here and is never granted
-- CONNECT on this database: it is the project plane's author role.
-- ---------------------------------------------------------------------------

-- Tenant authority is DATABASE-AUTHORITATIVE. `app.tenant` is a caller-set GUC,
-- so it can only ever be a consistency assertion; the row filter an author
-- cannot influence is this owner-maintained mapping from an exact login identity
-- to its one tenant. One management instance serves exactly one
-- (org_id, project_id, environment), and both of that scope's A/B generations map
-- to that scope's single tenant.
--
-- It deliberately carries NO row-level security. The resolver below is SECURITY
-- DEFINER and runs as the owner, and FORCE ROW LEVEL SECURITY applies to the
-- owner too — an enabled policy here would make every author session resolve
-- NULL and deny everything. Confinement is by ACL instead: nothing but the owner
-- reaches this relation, and the author is granted only EXECUTE on the resolver.
CREATE SCHEMA IF NOT EXISTS wamn_authority AUTHORIZATION wamn_system;
REVOKE ALL ON SCHEMA wamn_authority FROM PUBLIC;

CREATE TABLE IF NOT EXISTS wamn_authority.author_login_tenants (
    login_identity text NOT NULL CHECK (login_identity <> ''),
    tenant_id      text NOT NULL CHECK (tenant_id <> ''),
    org_id         text NOT NULL CHECK (org_id <> ''),
    project_id     text NOT NULL CHECK (project_id <> ''),
    environment    text NOT NULL CHECK (environment <> ''),
    created_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (login_identity)
);
REVOKE ALL ON TABLE wamn_authority.author_login_tenants FROM PUBLIC;

-- Fixed search path: the resolved tenant must not depend on a caller's
-- `search_path`, and the author has no privilege on the mapping relation itself.
-- An unmapped login resolves NULL, so `tenant_id = NULL` is NULL, so every
-- restrictive policy below refuses — absence of a mapping fails closed.
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

-- Every author-accessed relation carries an APPLICABLE restrictive policy.
-- RESTRICTIVE means it can only narrow: the permissive `app.tenant` policy above
-- still has to pass as well, so a caller that rewrites `app.tenant` can only
-- turn its own access OFF — it can never widen it, and it can never reach a row
-- its login is not mapped to. `TO wamn_control_author` is what makes the policy
-- applicable to the author without denying the owner, which still applies the
-- store as `wamn_system` under its own `app.tenant` claim.
DO $author_policies$
DECLARE
    relation_name text;
    policy_name text;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'catalog.flow_artifacts',
        'catalog.releases', 'catalog.release_flows',
        'catalog.catalog_heads', 'catalog.flow_drafts',
        'catalog.connection_requirements',
        'catalog.draft_safe_connection_grants', 'catalog.authoring_command_audit',
        'wamn_run.authoring_test_run_reservations',
        'wamn_run.authoring_test_case_runs', 'wamn_run.authoring_test_reports'
    ]
    LOOP
        policy_name := split_part(relation_name, '.', 2) || '_author_tenant';
        EXECUTE format(
            'DROP POLICY IF EXISTS %I ON %s',
            policy_name,
            relation_name
        );
        EXECUTE format(
            'CREATE POLICY %I ON %s AS RESTRICTIVE TO wamn_control_author USING (tenant_id = wamn_authority.session_author_tenant()) WITH CHECK (tenant_id = wamn_authority.session_author_tenant())',
            policy_name,
            relation_name
        );
    END LOOP;
END
$author_policies$;

-- The exact authority class. Explicit REVOKE-then-GRANT per relation so an
-- inherited or previously granted privilege cannot widen the set on replay.
REVOKE ALL PRIVILEGES ON SCHEMA catalog, wamn_run, wamn_authority
    FROM wamn_control_author;
GRANT USAGE ON SCHEMA catalog, wamn_run, wamn_authority TO wamn_control_author;
GRANT EXECUTE ON FUNCTION wamn_authority.session_author_tenant()
    TO wamn_control_author;

-- Portable catalog and draft-base facts the author only ever reads.
REVOKE ALL PRIVILEGES ON catalog.flow_artifacts, catalog.releases,
    catalog.release_flows, catalog.catalog_heads, catalog.connection_requirements,
    catalog.draft_safe_connection_grants FROM wamn_control_author;
GRANT SELECT ON catalog.flow_artifacts, catalog.releases,
    catalog.release_flows, catalog.catalog_heads, catalog.connection_requirements,
    catalog.draft_safe_connection_grants TO wamn_control_author;

-- The one mutable authored document: optimistic revision control, guarded by
-- flow_drafts_controlled_update, with DELETE structurally refused.
REVOKE ALL PRIVILEGES ON catalog.flow_drafts FROM wamn_control_author;
GRANT SELECT, INSERT, UPDATE ON catalog.flow_drafts TO wamn_control_author;

-- Append-only facts: immutable after append, enforced by their own triggers as
-- well as by the absence of UPDATE and DELETE here.
REVOKE ALL PRIVILEGES ON
    catalog.authoring_command_audit, wamn_run.authoring_test_reports
    FROM wamn_control_author;
GRANT SELECT, INSERT ON catalog.authoring_command_audit,
    wamn_run.authoring_test_reports
    TO wamn_control_author;

-- Reservation and case-map state machines: exactly the transitions they landed
-- with, never DELETE.
REVOKE ALL PRIVILEGES ON wamn_run.authoring_test_run_reservations,
    wamn_run.authoring_test_case_runs FROM wamn_control_author;
GRANT SELECT, INSERT, UPDATE ON wamn_run.authoring_test_run_reservations,
    wamn_run.authoring_test_case_runs TO wamn_control_author;

-- Owner-only is an explicit contract, not merely an absence of grants inherited
-- from a fresh database. Default PUBLIC function execution is revoked above.
REVOKE ALL ON ALL TABLES IN SCHEMA catalog, wamn_run FROM PUBLIC;

-- The author's bounded set is granted above; every other relation in these
-- schemas — deployment attestations, the release exposure / source / attachment
-- records, and the catalog registry itself — stays
-- owner-only for it too. Asserted rather than assumed, because a stray GRANT
-- here is exactly the mistake that would hand one principal another's plane.
DO $author_bounds$
DECLARE
    unexpected text;
BEGIN
    SELECT string_agg(format('%s:%s', relation, privilege), ', ' ORDER BY relation, privilege)
      INTO unexpected
      FROM (
        SELECT (quote_ident(namespace.nspname) || '.' || quote_ident(relation.relname))
                 AS relation,
               candidate.privilege
          FROM pg_class AS relation
          JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
          CROSS JOIN unnest(ARRAY['SELECT', 'INSERT', 'UPDATE', 'DELETE',
                                  'TRUNCATE', 'REFERENCES', 'TRIGGER'])
               AS candidate(privilege)
         WHERE relation.relkind IN ('r', 'p')
           AND namespace.nspname IN ('catalog', 'wamn_run', 'wamn_authority')
           AND (has_table_privilege('wamn_control_author', relation.oid,
                                    candidate.privilege)
                OR (candidate.privilege IN ('INSERT', 'UPDATE', 'REFERENCES')
                    AND has_any_column_privilege('wamn_control_author', relation.oid,
                                                 candidate.privilege)))
           AND NOT EXISTS (
             SELECT 1
               FROM (VALUES
                 ('catalog', 'flow_artifacts', 'SELECT'),
                 ('catalog', 'releases', 'SELECT'),
                 ('catalog', 'release_flows', 'SELECT'),
                 ('catalog', 'catalog_heads', 'SELECT'),
                 ('catalog', 'connection_requirements', 'SELECT'),
                 ('catalog', 'draft_safe_connection_grants', 'SELECT'),
                 ('catalog', 'flow_drafts', 'SELECT'),
                 ('catalog', 'flow_drafts', 'INSERT'),
                 ('catalog', 'flow_drafts', 'UPDATE'),
                 ('catalog', 'authoring_command_audit', 'SELECT'),
                 ('catalog', 'authoring_command_audit', 'INSERT'),
                 ('wamn_run', 'authoring_test_reports', 'SELECT'),
                 ('wamn_run', 'authoring_test_reports', 'INSERT'),
                 ('wamn_run', 'authoring_test_run_reservations', 'SELECT'),
                 ('wamn_run', 'authoring_test_run_reservations', 'INSERT'),
                 ('wamn_run', 'authoring_test_run_reservations', 'UPDATE'),
                 ('wamn_run', 'authoring_test_case_runs', 'SELECT'),
                 ('wamn_run', 'authoring_test_case_runs', 'INSERT'),
                 ('wamn_run', 'authoring_test_case_runs', 'UPDATE')
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
    IF pg_catalog.has_schema_privilege('wamn_control_author', 'catalog', 'CREATE')
       OR pg_catalog.has_schema_privilege('wamn_control_author', 'wamn_run', 'CREATE')
       OR pg_catalog.has_schema_privilege('wamn_control_author', 'wamn_authority',
                                          'CREATE')
       OR pg_catalog.has_function_privilege(
            'wamn_control_author',
            'catalog.register_deployment_attestation(text,text,int,text,text,text,text,timestamptz)',
            'EXECUTE')
       OR pg_catalog.pg_has_role('wamn_control_author', 'wamn_system', 'USAGE')
       OR EXISTS (SELECT FROM pg_catalog.pg_roles
                   WHERE rolname = 'wamn_scenario_author'
                     AND pg_catalog.has_database_privilege(
                           oid, pg_catalog.current_database(), 'CONNECT'))
    THEN
        RAISE EXCEPTION USING ERRCODE = '42501',
            MESSAGE = 'control-author-authority-boundary-violated';
    END IF;
END
$author_bounds$;
