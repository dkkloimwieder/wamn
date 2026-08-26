-- Portable authoring, release, and test storage for the T1
-- control database (wamn-0h0g.9.9). Apply after system-schema.sql as
-- wamn_system. This artifact is deliberately dormant: it grants no production
-- role access and installs no project/runtime compatibility path.
--
-- The qualified names are intentionally unchanged from the project copies.
-- During the one-cutover train both databases can therefore carry catalog.*;
-- database residency, not a renamed schema, distinguishes them. `wamn_run` holds
-- exactly ONE relation: wamn-0h0g.8.5.5 deleted the whole reservation-era
-- gate-report lineage that used to live in it and kept the schema, and
-- wamn-0h0g.8.5.6 put the surviving report row back in it -- keyed by
-- `wiring_hash`, construction against the surviving row rather than migration of
-- a corpse. The drift guard below asserts that exact inventory, so the schema
-- stays this artifact's exclusively.
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

-- NO DRAFT RELATION AT ALL (wamn-0h0g.8.5.5). wamn-pm7k retired the
-- validated-draft row on the grounds that the wiring document IS the validated
-- artifact and its hash IS the identity; this finishes the same thought for the
-- mutable half. A draft is a CLIENT-SIDE FILE -- a studio buffer, a git working
-- tree -- so a server-side revision counter, an `edited_at` ordering and a
-- stored `definition` were state the platform had no reason to own. The
-- monotonic-revision trigger function retires with the table it guarded.
--
-- `validated_draft_id` elsewhere in this artifact is read as the wiring hash and
-- needs no lineage row to resolve it.

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

-- No draft-safe connection-grant relation (wamn-0h0g.8.5.5): gate cases are
-- EFFECT-FREE BY CONTRACT, so a gate reaches no connection at all and there is
-- nothing left for a per-generation draft-safety grant to say. The relation
-- never carried production DML in any plane -- no INSERT, UPDATE or SELECT
-- existed for it anywhere -- so its entire surface was one authoring-role
-- privilege assertion about itself, and that retires with it.

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
        CHECK (command_kind IN ('test-set-run', 'publish')),
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
-- The ledger's command vocabulary is the CONTRACT's (wamn-0h0g.26.18), and
-- wamn-0h0g.8.5.5 collapsed five commands to two. The declaration above already
-- carries the narrowed vocabulary, but CREATE TABLE IF NOT EXISTS cannot reach a
-- store provisioned before the narrowing, and
-- `control-portable-retained-shape-drift` hashes this constraint's definition,
-- so the converging ALTER is what keeps an existing store applying. ADD
-- CONSTRAINT validates the heap directly, so a store still holding a
-- `save-draft`, `validate` or `draft-run` audit row refuses here by name rather
-- than carrying a vocabulary no command can produce.
--
-- `gate` is spelled `test-set-run` here for the same reason it is on the wire:
-- the literal follows the wiring vocabulary sweep (wamn-0h0g.26.18), not this
-- collapse.
ALTER TABLE catalog.authoring_command_audit
    DROP CONSTRAINT IF EXISTS authoring_command_audit_command_kind_check,
    ADD CONSTRAINT authoring_command_audit_command_kind_check
        CHECK (command_kind IN ('test-set-run', 'publish'));

-- The ONE durable fact an accepted gate produces (wamn-0h0g.8.5.6).
--
-- A gate is a JUDGMENT ABOUT A DOCUMENT and its cases are effect-free by
-- contract, so the verdict is reproducible from the document: same hash, same
-- report, byte-stable. The row is therefore keyed by `wiring_hash` ALONE and
-- mints no identity of its own -- the same collapse that deleted
-- `catalog.wirings.gate_report_id`, which was bare text with no foreign key
-- sitting beside a real content hash.
--
-- Per the 2026-08-25 standing rule, its writer and reader in one sentence: it is
-- WRITTEN by the gate verb at `services/scenario-worker/src/management.rs`, in
-- the same transaction as that command's ledger row, and READ by `get-report` at
-- `services/scenario-worker/src/authoring.rs`, both under the
-- `wamn_control_author` principal.
--
-- Only an ACCEPTED judgment writes here. A refusal is not a report, so an absent
-- row and `report-not-found` are the same fact. `passed` is nonetheless STORED
-- rather than inferred from that absence: `get-report` projects the store's
-- facts, and a projection must not synthesise one the store does not hold.
--
-- Re-gating the same document is idempotent by construction, so the writer
-- appends `ON CONFLICT DO NOTHING` against this key rather than rewriting a row
-- the immutability trigger below would refuse anyway.
CREATE TABLE IF NOT EXISTS wamn_run.gate_reports (
    tenant_id    text NOT NULL CHECK (tenant_id <> ''),
    wiring_hash  text NOT NULL
        CHECK (wiring_hash ~ '^sha256:[0-9a-f]{64}$'),
    passed       boolean NOT NULL,
    summary      jsonb NOT NULL CHECK (jsonb_typeof(summary) = 'object'),
    gated_at     timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, wiring_hash)
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

-- wamn-0h0g.26.21: the flow-era release plane retires whole. Its five relations
-- were a closed FK component -- attachments onto flows and sources, sources onto
-- the exposure manifest, flows onto the artifacts -- so nothing retained sat on
-- top of them and the rows are discarded without archive, exactly like the
-- bundle bytes below. Child first, RESTRICT throughout: the order is the FK
-- order and RESTRICT proves nothing outside the component came to depend on
-- them. Placed BEFORE $retire_execution_bundles$ because on an ancient database
-- catalog.release_flows carries the execution_bundle_hash FK that the bundle
-- block's RESTRICT drop would otherwise refuse on -- dropping the carrier
-- relation subsumes the DROP COLUMN that used to stand there.
DO $retire_flow_release_plane$
DECLARE
    retired_relation text;
BEGIN
    FOREACH retired_relation IN ARRAY ARRAY[
        'catalog.release_attachments',
        'catalog.release_sources',
        'catalog.release_exposure_manifests',
        'catalog.release_flows',
        'catalog.flow_artifacts'
    ]
    LOOP
        IF to_regclass(retired_relation) IS NOT NULL THEN
            EXECUTE format(
                'LOCK TABLE %s IN ACCESS EXCLUSIVE MODE', retired_relation
            );
            EXECUTE format('DROP TABLE %s RESTRICT', retired_relation);
        END IF;
    END LOOP;
END
$retire_flow_release_plane$;

-- Persisted bundle bytes are deliberately discarded without archive or
-- conversion. Drop every direct carrier first so the final RESTRICT drop is a
-- dependency tripwire, not an instruction to amputate an un-inventoried object.
DO $retire_execution_bundles$
BEGIN
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
        'catalog.catalogs',
        'catalog.releases',
        'catalog.catalog_heads',
        'catalog.component_library',
        'catalog.connection_requirements',
        'catalog.authoring_command_audit',
        'catalog.deployment_attestations',
        'wamn_run.gate_reports'
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
        'catalog.releases',
        'catalog.component_library',
        'catalog.connection_requirements', 'catalog.authoring_command_audit',
        'catalog.deployment_attestations',
        'wamn_run.gate_reports'
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

    -- wamn-0h0g.8.5.5: gate cases are effect-free by contract, so a gate
    -- reaches no connection and this relation's concept is void. It never had a
    -- writer or a production reader, so its rows name nothing and are discarded
    -- without archive exactly like the validated-draft rows above. RESTRICT
    -- because nothing may depend on it: the only reference it ever had was an
    -- authority probe asserting privileges on it, which retires in this commit.
    IF to_regclass('catalog.draft_safe_connection_grants') IS NOT NULL THEN
        LOCK TABLE catalog.draft_safe_connection_grants IN ACCESS EXCLUSIVE MODE;
        DROP TABLE catalog.draft_safe_connection_grants RESTRICT;
    END IF;

    -- wamn-0h0g.8.5.5: a draft is a client-side file, so the mutable document
    -- store has no writer and its rows name nothing -- they are discarded
    -- without archive, exactly like the validated-draft rows above. CASCADE is
    -- deliberately NOT used: RESTRICT proves nothing came to depend on it. The
    -- two triggers on the relation go with the relation itself; the trigger
    -- FUNCTION they called is dropped separately because a function outlives the
    -- triggers that referenced it.
    IF to_regclass('catalog.flow_drafts') IS NOT NULL THEN
        LOCK TABLE catalog.flow_drafts IN ACCESS EXCLUSIVE MODE;
        DROP TABLE catalog.flow_drafts RESTRICT;
    END IF;
    DROP FUNCTION IF EXISTS catalog.guard_flow_draft_update();

    -- wamn-0h0g.8.5.5: the whole run-plane gate-report lineage deletes. The
    -- owner ruling of 2026-08-25 struck the surviving-table clause: a relation
    -- whose only production writer (`insert_finalized_test_report_sql`) and only
    -- production reader (`select_test_report_projection_sql`) are deleted by the
    -- same change does not survive it. An effect-free gate's report is
    -- reproducible from the document, so none of this was durable state worth
    -- keeping -- it was the composition machinery's memory for per-ordinal
    -- resumption, and the effect-free clause deleted the thing it remembered.
    --
    -- Child first: both records carry a foreign key into the reservation, so the
    -- parent's RESTRICT drop would refuse while either stands. RESTRICT
    -- throughout, never CASCADE: it proves nothing else came to depend on them.
    -- The rows are discarded without archive exactly like the draft rows above.
    IF to_regclass('wamn_run.authoring_test_reports') IS NOT NULL THEN
        LOCK TABLE wamn_run.authoring_test_reports IN ACCESS EXCLUSIVE MODE;
        DROP TABLE wamn_run.authoring_test_reports RESTRICT;
    END IF;
    IF to_regclass('wamn_run.authoring_test_case_runs') IS NOT NULL THEN
        LOCK TABLE wamn_run.authoring_test_case_runs IN ACCESS EXCLUSIVE MODE;
        DROP TABLE wamn_run.authoring_test_case_runs RESTRICT;
    END IF;
    IF to_regclass('wamn_run.authoring_test_run_reservations') IS NOT NULL THEN
        LOCK TABLE wamn_run.authoring_test_run_reservations
            IN ACCESS EXCLUSIVE MODE;
        DROP TABLE wamn_run.authoring_test_run_reservations RESTRICT;
    END IF;

    -- wamn-0h0g.15.159 dropped the sealed member snapshot from the release
    -- identity row. This is a RETAINED relation, so DROP COLUMN cannot reach
    -- the asserted shape:
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
        'releases'
    ]::text[] THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'control-portable-catalog-inventory-drift';
    END IF;

    -- The guard's job is to refuse an inventory this artifact did not put
    -- there, and that job is unchanged by which relations it declares: the
    -- schema belongs exclusively to this store, so a relation appearing in it
    -- is drift. wamn-0h0g.8.5.5 emptied the declared set; wamn-0h0g.8.5.6 put
    -- the one surviving report row back, so the asserted inventory is that one
    -- name. It is spelled against a literal array rather than `IS NOT NULL`
    -- again now that the set is non-empty -- an inventory of the WRONG one name
    -- must refuse exactly as loudly as an extra one.
    SELECT array_agg(tablename ORDER BY tablename) INTO run_tables
    FROM pg_tables WHERE schemaname = 'wamn_run';
    IF run_tables IS DISTINCT FROM ARRAY['gate_reports']::text[] THEN
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
            ('catalog.catalogs'),
            ('catalog.releases'),
            ('catalog.catalog_heads'),
            ('catalog.component_library'),
            ('catalog.connection_requirements'),
            ('catalog.authoring_command_audit'),
            ('wamn_run.gate_reports')
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
    )
    SELECT encode(sha256(convert_to(string_agg(
        relation || '|' || fact,
        E'\n' ORDER BY relation COLLATE "C", fact COLLATE "C"
    ), 'UTF8')), 'hex')
    INTO retained_fingerprint
    FROM facts;
    IF retained_fingerprint <>
       '2bbd219e98ae3fb68b6bb647607968cf6b033c89ad9c6f5edece975cb356ec61'
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

-- DATED DECISION, wamn-0h0g.9.14 (2026-08-26). This is a MAPPING TABLE, and the
-- guest-path ruling on wamn-0h0g.22.6 BANS that shape: it rejected mapping
-- tables because the prohibition targets mutable, settable state between
-- identity and authority, and gave guests a pure immutable derivation
-- (`wamn.tenant_key`) instead. That argument is about the SHAPE, so it applies
-- here on its face. It is ACCEPTED FOR MVP on the bounded-principal argument —
-- control-plane authors are a bounded, platform-administered set, where tenant
-- guest code is unbounded and adversarial — and because the real remedy is
-- per-author login generations carrying the tenant in the role identity, not a
-- rename (a table-reading function cannot become IMMUTABLE). It REOPENS on
-- either trigger: any widening of who can hold author logins, or the first
-- control-plane multi-tenancy expansion. This is a recorded decision, not an
-- oversight; read wamn-0h0g.9.14 before "fixing" it.
--
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
        'catalog.releases',
        'catalog.catalog_heads',
        'catalog.connection_requirements',
        'catalog.authoring_command_audit',
        'wamn_run.gate_reports'
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
REVOKE ALL PRIVILEGES ON catalog.releases,
    catalog.catalog_heads, catalog.connection_requirements
    FROM wamn_control_author;
GRANT SELECT ON catalog.releases,
    catalog.catalog_heads, catalog.connection_requirements
    TO wamn_control_author;

-- The author holds NO mutable-document authority at all (wamn-0h0g.8.5.5): the
-- one relation it could write in place is deleted, so every grant below is over
-- a read or an append-only fact.

-- Append-only facts: immutable after append, enforced by their own triggers as
-- well as by the absence of UPDATE and DELETE here.
--
-- wamn-0h0g.8.5.5 left exactly one and wamn-0h0g.8.5.6 added the second. The
-- reservation-era gate-report lineage was the author's only STATE-MACHINE
-- authority here and its three relations are deleted above; the report row that
-- replaced them is written once and never revised, so the author still holds no
-- transition authority at all -- every grant it has is a read or an append.
REVOKE ALL PRIVILEGES ON catalog.authoring_command_audit, wamn_run.gate_reports
    FROM wamn_control_author;
GRANT SELECT, INSERT ON catalog.authoring_command_audit, wamn_run.gate_reports
    TO wamn_control_author;

-- Owner-only is an explicit contract, not merely an absence of grants inherited
-- from a fresh database. Default PUBLIC function execution is revoked above.
REVOKE ALL ON ALL TABLES IN SCHEMA catalog, wamn_run FROM PUBLIC;

-- The author's bounded set is granted above; every other relation in these
-- schemas — deployment attestations and the catalog registry itself — stays
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
                 ('catalog', 'releases', 'SELECT'),
                 ('catalog', 'catalog_heads', 'SELECT'),
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
