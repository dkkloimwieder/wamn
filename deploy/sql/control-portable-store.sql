-- Portable authoring, release, test, and execution-plan storage for the T1
-- control database (wamn-0h0g.9.9). Apply after system-schema.sql as
-- wamn_system. This artifact is deliberately dormant: it grants no production
-- role access and installs no project/runtime compatibility path.
--
-- The qualified names are intentionally unchanged from the project copies.
-- During the one-cutover train both databases can therefore carry catalog.*
-- and wamn_run.authoring_test_*; database residency, not a renamed schema,
-- distinguishes them.
--
-- `control-portable-retained-shape-drift` and the release-evidence constraint
-- fingerprint below are apply-time digests: they must be regenerated whenever
-- a retained relation's shape moves (wamn-0h0g.15.22).

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

CREATE TABLE IF NOT EXISTS catalog.execution_bundles (
    tenant_id             text NOT NULL CHECK (tenant_id <> ''),
    execution_bundle_hash text NOT NULL
        CHECK (execution_bundle_hash ~ '^sha256:[0-9a-f]{64}$'),
    format_version        text NOT NULL CHECK (format_version = '0.1'),
    exact_bytes           bytea NOT NULL,
    byte_length           int NOT NULL CHECK (byte_length = octet_length(exact_bytes)),
    created_at            timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, execution_bundle_hash),
    CONSTRAINT execution_bundles_exact_hash CHECK (
        execution_bundle_hash = 'sha256:' || encode(sha256(exact_bytes), 'hex')
    )
);

CREATE TABLE IF NOT EXISTS catalog.release_manifests (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    catalog_id      text NOT NULL,
    catalog_version int NOT NULL,
    members_json    jsonb NOT NULL CHECK (jsonb_typeof(members_json) = 'array'),
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
    execution_bundle_hash text NOT NULL
        CHECK (execution_bundle_hash ~ '^sha256:[0-9a-f]{64}$'),
    PRIMARY KEY (tenant_id, catalog_id, catalog_version, flow_id),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.release_manifests (tenant_id, catalog_id, catalog_version),
    FOREIGN KEY (tenant_id, flow_id, flow_version)
        REFERENCES catalog.flow_artifacts (tenant_id, flow_id, flow_version),
    FOREIGN KEY (tenant_id, execution_bundle_hash)
        REFERENCES catalog.execution_bundles (tenant_id, execution_bundle_hash)
);
CREATE INDEX IF NOT EXISTS release_flows_execution_bundle
    ON catalog.release_flows (tenant_id, execution_bundle_hash);

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

CREATE TABLE IF NOT EXISTS catalog.validated_flow_drafts (
    tenant_id                  text NOT NULL CHECK (tenant_id <> ''),
    draft_id                   text NOT NULL CHECK (draft_id <> ''),
    draft_revision             bigint NOT NULL CHECK (draft_revision > 0),
    draft_edited_at            timestamptz NOT NULL,
    draft_content_hash         text NOT NULL CHECK (draft_content_hash <> ''),
    catalog_id                 text NOT NULL CHECK (catalog_id <> ''),
    catalog_version            int NOT NULL CHECK (catalog_version > 0),
    environment                text NOT NULL CHECK (environment <> ''),
    flow_id                    text NOT NULL CHECK (flow_id <> ''),
    runtime_flow_version       int NOT NULL CHECK (runtime_flow_version > 0),
    graph_json                 jsonb NOT NULL CHECK (jsonb_typeof(graph_json) = 'object'),
    graph_hash                 text NOT NULL CHECK (graph_hash <> ''),
    draft_artifact_hash        text NOT NULL CHECK (draft_artifact_hash <> ''),
    execution_bundle_hash      text NOT NULL
        CHECK (execution_bundle_hash ~ '^sha256:[0-9a-f]{64}$'),
    binding_base_artifact_hash text NOT NULL CHECK (binding_base_artifact_hash <> ''),
    validated_draft_hash       text NOT NULL CHECK (validated_draft_hash <> ''),
    validated_at               timestamptz NOT NULL DEFAULT now(),
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

CREATE TABLE IF NOT EXISTS catalog.release_exposure_manifests (
    tenant_id        text NOT NULL CHECK (tenant_id <> ''),
    catalog_id       text NOT NULL,
    catalog_version  int NOT NULL,
    definitions_json jsonb NOT NULL CHECK (jsonb_typeof(definitions_json) = 'object'),
    PRIMARY KEY (tenant_id, catalog_id, catalog_version),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.release_manifests (tenant_id, catalog_id, catalog_version)
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
    artifact_hash    text NOT NULL CHECK (artifact_hash <> ''),
    requirement_name text NOT NULL CHECK (requirement_name <> ''),
    requirement_json jsonb NOT NULL CHECK (jsonb_typeof(requirement_json) = 'object'),
    requirement_hash text NOT NULL CHECK (requirement_hash <> ''),
    created_at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, artifact_hash, requirement_name)
);

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
        CHECK (command_kind IN ('save-flow-draft', 'validate', 'draft-run',
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

CREATE TABLE IF NOT EXISTS wamn_run.authoring_test_run_reservations (
    tenant_id          text NOT NULL CHECK (tenant_id <> ''),
    report_id          text NOT NULL CHECK (report_id <> ''),
    command_hash       text NOT NULL CHECK (command_hash ~ '^sha256:[0-9a-f]{64}$'),
    validated_draft_id text NOT NULL CHECK (validated_draft_id <> ''),
    catalog_id         text NOT NULL CHECK (catalog_id <> ''),
    catalog_version    int NOT NULL CHECK (catalog_version > 0),
    case_count         int NOT NULL CHECK (case_count BETWEEN 1 AND 256),
    resolution_map     jsonb CHECK (
        resolution_map IS NULL OR jsonb_typeof(resolution_map) = 'object'
    ),
    resolution_map_hash text CHECK (
        resolution_map_hash IS NULL OR resolution_map_hash ~ '^sha256:[0-9a-f]{64}$'
    ),
    state             text NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'finalized')),
    created_at        timestamptz NOT NULL DEFAULT clock_timestamp(),
    whole_deadline_at timestamptz NOT NULL,
    finalized_at      timestamptz,
    PRIMARY KEY (tenant_id, report_id),
    UNIQUE (tenant_id, report_id, catalog_id, catalog_version, validated_draft_id),
    CHECK ((resolution_map IS NULL) = (resolution_map_hash IS NULL)),
    CHECK (
        resolution_map IS NULL
        OR resolution_map_hash = 'sha256:' || encode(
            sha256(convert_to(resolution_map::text, 'UTF8')), 'hex'
        )
    ),
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
    resolution_map      jsonb CHECK (
        resolution_map IS NULL OR jsonb_typeof(resolution_map) = 'object'
    ),
    resolution_map_hash text CHECK (
        resolution_map_hash IS NULL OR resolution_map_hash ~ '^sha256:[0-9a-f]{64}$'
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
    CHECK ((resolution_map IS NULL) = (resolution_map_hash IS NULL)),
    CHECK (
        resolution_map IS NULL
        OR resolution_map_hash = 'sha256:' || encode(
            sha256(convert_to(resolution_map::text, 'UTF8')), 'hex'
        )
    ),
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
    resolution_map      jsonb NOT NULL CHECK (jsonb_typeof(resolution_map) = 'object'),
    resolution_map_hash text NOT NULL CHECK (resolution_map_hash ~ '^sha256:[0-9a-f]{64}$'),
    passed              boolean NOT NULL,
    summary             jsonb NOT NULL CHECK (jsonb_typeof(summary) = 'object'),
    finalized_at        timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, report_id),
    FOREIGN KEY (tenant_id, report_id)
        REFERENCES wamn_run.authoring_test_run_reservations (tenant_id, report_id),
    CHECK (
        resolution_map_hash = 'sha256:' || encode(
            sha256(convert_to(resolution_map::text, 'UTF8')), 'hex'
        )
    )
);

CREATE TABLE IF NOT EXISTS catalog.release_flow_test_evidence (
    tenant_id                  text NOT NULL CHECK (tenant_id <> ''),
    catalog_id                 text NOT NULL CHECK (catalog_id <> ''),
    catalog_version            int NOT NULL CHECK (catalog_version > 0),
    flow_id                    text NOT NULL CHECK (flow_id <> ''),
    validated_draft_id         text NOT NULL CHECK (validated_draft_id <> ''),
    report_id                  text NOT NULL CHECK (report_id <> ''),
    test_set_hash              text NOT NULL CHECK (test_set_hash ~ '^sha256:[0-9a-f]{64}$'),
    source_artifact_hash       text NOT NULL CHECK (source_artifact_hash <> ''),
    execution_bundle_hash      text NOT NULL
        CHECK (execution_bundle_hash ~ '^sha256:[0-9a-f]{64}$'),
    tested_resolution_map_bytes bytea NOT NULL,
    tested_resolution_map_hash text NOT NULL
        CHECK (tested_resolution_map_hash ~ '^sha256:[0-9a-f]{64}$'),
    created_at                 timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, catalog_id, catalog_version, flow_id),
    FOREIGN KEY (tenant_id, catalog_id, catalog_version, flow_id)
        REFERENCES catalog.release_flows
            (tenant_id, catalog_id, catalog_version, flow_id),
    FOREIGN KEY (tenant_id, validated_draft_id)
        REFERENCES catalog.validated_flow_drafts (tenant_id, validated_draft_hash),
    FOREIGN KEY (tenant_id, report_id)
        REFERENCES wamn_run.authoring_test_reports (tenant_id, report_id),
    FOREIGN KEY (tenant_id, execution_bundle_hash)
        REFERENCES catalog.execution_bundles (tenant_id, execution_bundle_hash),
    CONSTRAINT release_flow_test_evidence_map_hash_check CHECK (
        tested_resolution_map_hash = 'sha256:' || encode(
            sha256(tested_resolution_map_bytes), 'hex'
        )
    )
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
        REFERENCES catalog.release_manifests (tenant_id, catalog_id, catalog_version)
);

CREATE OR REPLACE FUNCTION catalog.register_release_flow_test_evidence(
    p_tenant_id text,
    p_catalog_id text,
    p_catalog_version int,
    p_flow_id text,
    p_validated_draft_id text,
    p_report_id text,
    p_test_set_hash text,
    p_source_artifact_hash text,
    p_execution_bundle_hash text,
    p_tested_resolution_map_bytes bytea,
    p_tested_resolution_map_hash text
)
RETURNS timestamptz
LANGUAGE plpgsql
AS $$
DECLARE
    existing_created_at timestamptz;
BEGIN
    INSERT INTO catalog.release_flow_test_evidence (
        tenant_id, catalog_id, catalog_version, flow_id,
        validated_draft_id, report_id, test_set_hash, source_artifact_hash,
        execution_bundle_hash, tested_resolution_map_bytes,
        tested_resolution_map_hash
    ) VALUES (
        p_tenant_id, p_catalog_id, p_catalog_version, p_flow_id,
        p_validated_draft_id, p_report_id, p_test_set_hash,
        p_source_artifact_hash, p_execution_bundle_hash,
        p_tested_resolution_map_bytes, p_tested_resolution_map_hash
    )
    ON CONFLICT (tenant_id, catalog_id, catalog_version, flow_id) DO NOTHING
    RETURNING created_at INTO existing_created_at;

    IF existing_created_at IS NOT NULL THEN
        RETURN existing_created_at;
    END IF;

    SELECT created_at INTO existing_created_at
    FROM catalog.release_flow_test_evidence
    WHERE tenant_id = p_tenant_id
      AND catalog_id = p_catalog_id
      AND catalog_version = p_catalog_version
      AND flow_id = p_flow_id
      AND validated_draft_id = p_validated_draft_id
      AND report_id = p_report_id
      AND test_set_hash = p_test_set_hash
      AND source_artifact_hash = p_source_artifact_hash
      AND execution_bundle_hash = p_execution_bundle_hash
      AND tested_resolution_map_bytes = p_tested_resolution_map_bytes
      AND tested_resolution_map_hash = p_tested_resolution_map_hash;

    IF existing_created_at IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '23505',
            MESSAGE = 'release-flow-test-evidence-content-conflict';
    END IF;
    RETURN existing_created_at;
END
$$;
REVOKE ALL ON FUNCTION catalog.register_release_flow_test_evidence(
    text, text, int, text, text, text, text, text, text, bytea, text
) FROM PUBLIC;

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

-- Tenant isolation is structural even while the dormant tables are owner-only.
DO $policies$
DECLARE
    relation_name text;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'catalog.catalogs', 'catalog.flow_artifacts',
        'catalog.execution_bundles', 'catalog.release_manifests',
        'catalog.release_flows', 'catalog.catalog_heads',
        'catalog.flow_drafts', 'catalog.validated_flow_drafts',
        'catalog.release_exposure_manifests', 'catalog.release_sources',
        'catalog.release_attachments', 'catalog.connection_requirements',
        'catalog.draft_safe_connection_grants', 'catalog.authoring_command_audit',
        'catalog.release_flow_test_evidence', 'catalog.deployment_attestations',
        'wamn_run.authoring_test_run_reservations',
        'wamn_run.authoring_test_case_runs', 'wamn_run.authoring_test_reports'
    ]
    LOOP
        EXECUTE format('ALTER TABLE %s ENABLE ROW LEVEL SECURITY', relation_name);
        EXECUTE format('ALTER TABLE %s FORCE ROW LEVEL SECURITY', relation_name);
        IF NOT EXISTS (
            SELECT 1
            FROM pg_policy
            WHERE polrelid = relation_name::regclass
              AND polname = replace(split_part(relation_name, '.', 2), '.', '_') || '_tenant'
        ) THEN
            EXECUTE format(
                'CREATE POLICY %I ON %s USING (tenant_id = NULLIF(current_setting(''app.tenant'', true), '''')) WITH CHECK (tenant_id = NULLIF(current_setting(''app.tenant'', true), ''''))',
                split_part(relation_name, '.', 2) || '_tenant',
                relation_name
            );
        END IF;
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
        'catalog.flow_artifacts', 'catalog.execution_bundles',
        'catalog.release_manifests', 'catalog.release_flows',
        'catalog.validated_flow_drafts', 'catalog.release_exposure_manifests',
        'catalog.release_sources', 'catalog.release_attachments',
        'catalog.connection_requirements', 'catalog.authoring_command_audit',
        'catalog.release_flow_test_evidence', 'catalog.deployment_attestations',
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

-- Replay is a reconcile, not a best-effort `IF NOT EXISTS`: refuse a relation
-- inventory or either newly ratified record shape that differs from this
-- artifact. Focused Rust drift guards pin retained-copy columns to the project
-- source while that source remains authoritative during the cutover train.
DO $drift$
DECLARE
    catalog_tables text[];
    run_tables text[];
    evidence_columns text[];
    attestation_columns text[];
    evidence_constraints_fingerprint text;
    attestation_constraints_fingerprint text;
    retained_fingerprint text;
BEGIN
    SELECT array_agg(tablename ORDER BY tablename) INTO catalog_tables
    FROM pg_tables WHERE schemaname = 'catalog';
    IF catalog_tables IS DISTINCT FROM ARRAY[
        'authoring_command_audit', 'catalog_heads', 'catalogs',
        'connection_requirements', 'deployment_attestations',
        'draft_safe_connection_grants', 'execution_bundles', 'flow_artifacts',
        'flow_drafts', 'release_attachments', 'release_exposure_manifests',
        'release_flow_test_evidence', 'release_flows', 'release_manifests',
        'release_sources', 'validated_flow_drafts'
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
    ) INTO evidence_columns
    FROM pg_attribute a
    WHERE a.attrelid = 'catalog.release_flow_test_evidence'::regclass
      AND a.attnum > 0 AND NOT a.attisdropped;
    IF evidence_columns IS DISTINCT FROM ARRAY[
        'tenant_id:text:true', 'catalog_id:text:true',
        'catalog_version:integer:true', 'flow_id:text:true',
        'validated_draft_id:text:true', 'report_id:text:true',
        'test_set_hash:text:true', 'source_artifact_hash:text:true',
        'execution_bundle_hash:text:true',
        'tested_resolution_map_bytes:bytea:true',
        'tested_resolution_map_hash:text:true',
        'created_at:timestamp with time zone:true'
    ]::text[] THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'release-flow-test-evidence-shape-drift';
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
        con.contype::text || ':' || pg_get_constraintdef(con.oid, false),
        E'\n' ORDER BY (con.contype::text || ':'
        || pg_get_constraintdef(con.oid, false)) COLLATE "C"
    ), 'UTF8')), 'hex')
    INTO evidence_constraints_fingerprint
    FROM pg_constraint con
    WHERE con.conrelid = 'catalog.release_flow_test_evidence'::regclass
      AND con.contype <> 'n';
    IF evidence_constraints_fingerprint <>
       '7e6f31e287802d22eea4a7320a072471a793b94fe3882e4e8bbc30fd981bd7ed'
    THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'release-flow-test-evidence-constraint-drift';
    END IF;

    SELECT encode(sha256(convert_to(string_agg(
        con.contype::text || ':' || pg_get_constraintdef(con.oid, true),
        E'\n' ORDER BY con.contype::text || ':'
        || pg_get_constraintdef(con.oid, true)
    ), 'UTF8')), 'hex')
    INTO attestation_constraints_fingerprint
    FROM pg_constraint con
    WHERE con.conrelid = 'catalog.deployment_attestations'::regclass
      AND con.contype <> 'n';
    IF attestation_constraints_fingerprint <>
       '402504526c60def1fddf35860ec2829c7b516837292b10a9e35ed377b8af9745'
    THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'deployment-attestation-constraint-drift';
    END IF;

    WITH retained_relations(relation) AS (
        VALUES
            ('catalog.catalogs'), ('catalog.flow_artifacts'),
            ('catalog.execution_bundles'), ('catalog.release_manifests'),
            ('catalog.release_flows'), ('catalog.catalog_heads'),
            ('catalog.flow_drafts'), ('catalog.validated_flow_drafts'),
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
        relation || '|' || fact, E'\n' ORDER BY relation, fact
    ), 'UTF8')), 'hex')
    INTO retained_fingerprint
    FROM facts;
    IF retained_fingerprint <>
       '91f3ffe851e16145aa96b6e2f1ccf56da70fafa300e238641e74ea524552dfab'
    THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'control-portable-retained-shape-drift';
    END IF;
END
$drift$;

-- Owner-only is an explicit contract, not merely an absence of grants inherited
-- from a fresh database. Default PUBLIC function execution is revoked above.
REVOKE ALL ON ALL TABLES IN SCHEMA catalog, wamn_run FROM PUBLIC;
