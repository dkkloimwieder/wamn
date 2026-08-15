-- Run-state storage schema (5.7). The production tables that PERSIST flow
-- execution: `runs` (one row per execution) and `node_runs` (one row per node
-- execution). This is the durable, queryable record behind run history, at-
-- least-once execution and immutable run/node history. The pure engine
-- (crates/execution/flow-engine, 5.2) is a single-shot in-memory reducer; these
-- tables are the facts the driver (components/execution/flowrunner) writes.
--
-- STANDALONE ARTIFACT: deliberately NOT included by deploy/sql/postgres-init.sql, the
-- same convention as deploy/sql/catalog-schema.sql (3.1/3.4/3.5/3.6). The S3/S6 gate
-- fixtures carry their own `runs`/`node_runs` copies (postgres-init.sql schema
-- `s3`) so flowbench exercises the rewired runner; this file is the production schema and the
-- target of the crate's live-apply gate. Assumes pre-existing `wamn_app`,
-- `wamn_scenario_author`, stable `wamn_effect_writer`, and stable
-- `wamn_run_projection_writer` ACL roles. Role and
-- scoped LOGIN credential-generation lifecycle is provisioning-owned; this
-- artifact grants the stable role ledger append/read authority plus only the
-- narrow run columns needed for its fenced runnable-state recheck.
--
-- Security shape mirrors the rest of the platform (s2/s3, catalog): tenant
-- separation purely via the `app.tenant` claim the wamn:postgres plugin injects
-- with SET LOCAL. Every table FORCEs RLS keyed on
-- NULLIF(current_setting('app.tenant', true), ''), which is NULL (=> zero rows)
-- when no claim was injected. Postgres resets a custom GUC to '' (not NULL)
-- after SET LOCAL scope ends, so NULLIF folds an empty claim to NULL; and
-- CHECK (tenant_id <> '') forbids a ''-tenant row, so an empty claim matches
-- nothing structurally, not just by convention.
--
-- SCOPE (what 5.7 does NOT own, reserved as nullable seams below): the durable
-- run QUEUE + leases + doorbell (5.14) co-transact with these INSERTs but own
-- their own table; the run-level I/O CAPTURE policy (9.6) is fixed at admission
-- and full capture fills scrubbed `input_json`/`output_json` plus the output
-- size and optional hash; the content-addressed payload BYTE store (5.10) is
-- pointed at by `input_ref`/`output_ref`.

CREATE SCHEMA IF NOT EXISTS wamn_run AUTHORIZATION CURRENT_USER;
REVOKE ALL PRIVILEGES ON SCHEMA wamn_run
    FROM PUBLIC, wamn_effect_writer, wamn_run_projection_writer;
GRANT USAGE ON SCHEMA wamn_run TO wamn_app;
GRANT USAGE ON SCHEMA wamn_run TO wamn_scenario_author;
GRANT USAGE ON SCHEMA wamn_run TO wamn_effect_writer;
GRANT USAGE ON SCHEMA wamn_run TO wamn_run_projection_writer;

-- Final admission must share-lock the stable catalog head, but the application
-- role must never gain UPDATE privilege on that control-plane row. This narrow
-- SECURITY DEFINER bridge takes only the row-share lock and returns the applied
-- version while rechecking the session tenant claim.
-- SHARE deliberately conflicts with the publisher's non-key pointer UPDATE;
-- KEY SHARE would allow admission to commit after the applied head moved.
CREATE FUNCTION wamn_run.lock_catalog_head(
    p_tenant_id text,
    p_catalog_id text,
    p_environment text
)
RETURNS int
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, catalog
AS $$
DECLARE
    applied_version int;
BEGIN
    SELECT head.applied_catalog_version INTO applied_version
    FROM catalog.catalog_heads AS head
    WHERE p_tenant_id = NULLIF(current_setting('app.tenant', true), '')
      AND head.tenant_id = p_tenant_id
      AND head.catalog_id = p_catalog_id
      AND head.environment = p_environment
    FOR SHARE OF head;
    RETURN applied_version;
END
$$;
REVOKE ALL ON FUNCTION wamn_run.lock_catalog_head(text, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION wamn_run.lock_catalog_head(text, text, text) TO wamn_app;
GRANT EXECUTE ON FUNCTION wamn_run.lock_catalog_head(text, text, text)
    TO wamn_scenario_author;

-- Effect attempt, dispatch, and outcome facts are immutable even for their
-- owning role; retention requires a future explicit ledger protocol.
CREATE FUNCTION wamn_run.reject_immutable_effect_fact_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = 'effect-fact-immutable';
END
$$;
REVOKE ALL ON FUNCTION wamn_run.reject_immutable_effect_fact_change() FROM PUBLIC;

-- Operator actions are immutable evidence and never follow run-history
-- pruning. Only the schema-owning project-admin path can append them.
CREATE FUNCTION wamn_run.reject_immutable_operator_run_action_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = 'operator-run-action-immutable';
END
$$;
REVOKE ALL ON FUNCTION wamn_run.reject_immutable_operator_run_action_change()
    FROM PUBLIC;

-- Causation is fixed by the admission transaction. Normal run-state updates
-- may advance status/checkpoints, but cannot rewrite event ancestry.
CREATE FUNCTION wamn_run.guard_event_lineage_immutable()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.event_source_run_id IS DISTINCT FROM OLD.event_source_run_id
       OR NEW.event_root_run_id IS DISTINCT FROM OLD.event_root_run_id
       OR NEW.event_depth IS DISTINCT FROM OLD.event_depth THEN
        RAISE EXCEPTION 'event causation lineage is immutable';
    END IF;
    RETURN NEW;
END
$$;

CREATE FUNCTION wamn_run.guard_run_admission_pins_immutable()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.catalog_id IS DISTINCT FROM OLD.catalog_id
       OR NEW.catalog_version IS DISTINCT FROM OLD.catalog_version
       OR NEW.environment IS DISTINCT FROM OLD.environment
       OR NEW.execution_bundle_hash IS DISTINCT FROM OLD.execution_bundle_hash
       OR NEW.capture_mode IS DISTINCT FROM OLD.capture_mode THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'run-admission-pin-immutable';
    END IF;
    RETURN NEW;
END
$$;

CREATE FUNCTION wamn_run.reject_immutable_flow_resolution_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = 'run-flow-resolution-immutable';
END
$$;
REVOKE ALL ON FUNCTION wamn_run.reject_immutable_flow_resolution_change() FROM PUBLIC;

-- ---------------------------------------------------------------------------
-- runs: one row per flow execution. `input_json` is the admitted trigger
-- payload; `result_json` is the last node's output on
-- completion; `state_json` carries transient run state such as bounded-retry
-- scheduling and execution context. `idempotency_key` dedupes at-least-once
-- redelivery of the same trigger (a partial-unique index). `fail_kind` mirrors
-- the engine `FailKind` so history can
-- flag an upstream bug (`invalid-input`) apart from a terminal error or an
-- exhausted retry budget. Status values are exactly wamn_run_store::RunStatus
-- as_sql (tied to the crate by a drift-guard test).
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.runs (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    run_id          text NOT NULL,
    flow_id         text NOT NULL,
    flow_version    int  NOT NULL,
    catalog_id      text NOT NULL,
    catalog_version int NOT NULL,
    environment     text NOT NULL,
    execution_bundle_hash text NOT NULL,
    attachment_id   text,
    registration_id text,
    -- Trusted immutable CDC event ancestry is distinct from retired execution
    -- lineage. The immediate source lets admission verify the carried root/depth
    -- under tenant RLS.
    event_source_run_id text,
    event_root_run_id text,
    event_depth      int CHECK (event_depth BETWEEN 0 AND 16),
    status          text NOT NULL DEFAULT 'running'
        CHECK (status IN ('dispatched', 'running', 'completed', 'failed',
                          'infrastructure-failure', 'effect-uncertain')),
    trigger_source  text,
    capture_mode    text NOT NULL DEFAULT 'off'
        CONSTRAINT runs_capture_mode_check CHECK (capture_mode IN ('full', 'off')),
    input_json      jsonb,
    result_json     jsonb,
    state_json      jsonb,
    invocation_context jsonb NOT NULL DEFAULT '{}'::jsonb,
    admission_context_version text NOT NULL DEFAULT '0.1'
        CHECK (admission_context_version = '0.1'),
    platform_revision text NOT NULL DEFAULT 'legacy',
    idempotency_key text,
    caller_outcome_kind text
        CHECK (caller_outcome_kind IN ('responded', 'failed')),
    caller_outcome_json jsonb,
    caller_http_status int CHECK (caller_http_status BETWEEN 100 AND 599),
    caller_release_node_id text,
    caller_outcome_hash text,
    caller_released_at timestamptz,
    response_deadline_at timestamptz,
    run_deadline_at timestamptz,
    terminal_reason text,
    fail_kind       text CHECK (fail_kind IN ('terminal', 'retry-exhausted', 'invalid-input',
                                              'runaway-budget', 'effect-uncertain', 'depth-budget',
                                              'dispatch-budget', 'unresolvable-name',
                                              'hash-invalid-bytes', 'foreign-revision',
                                              'incompatible-contract', 'unbound-requirement')),
    fail_node       text,
    fail_reason     text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (catalog_id <> ''
           AND catalog_version > 0
           AND environment <> ''
           AND execution_bundle_hash ~ '^sha256:[0-9a-f]{64}$'),
    CHECK (jsonb_typeof(invocation_context) = 'object'
           AND octet_length(invocation_context::text) <= 16384),
    CHECK (
      (event_source_run_id IS NULL AND event_root_run_id IS NULL AND event_depth IS NULL)
      OR
      (trigger_source = 'event'
       AND event_source_run_id IS NOT NULL AND event_source_run_id <> ''
       AND event_root_run_id IS NOT NULL AND event_root_run_id <> ''
       AND event_depth IS NOT NULL)
    ),
    CHECK (
      event_depth IS DISTINCT FROM 0
      OR (event_source_run_id = run_id AND event_root_run_id = run_id)
    ),
    CONSTRAINT runs_check6
        CHECK ((caller_released_at IS NULL) = (caller_outcome_kind IS NULL)),
    CONSTRAINT runs_check7
        CHECK (caller_outcome_kind IS NULL OR caller_outcome_json IS NOT NULL),
    CONSTRAINT runs_check8
        CHECK (caller_outcome_kind <> 'responded' OR caller_release_node_id IS NOT NULL),
    CONSTRAINT runs_check9
        CHECK (response_deadline_at IS NULL OR run_deadline_at IS NULL
               OR response_deadline_at <= run_deadline_at),
    CONSTRAINT runs_capture_mode_source_check CHECK (
      capture_mode <> 'full' OR trigger_source IS NOT DISTINCT FROM 'scenario-draft'
    ),
    PRIMARY KEY (tenant_id, run_id),
    CONSTRAINT runs_release_fk
        FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.release_manifests (tenant_id, catalog_id, catalog_version),
    CONSTRAINT runs_execution_bundle_fk
        FOREIGN KEY (tenant_id, execution_bundle_hash)
        REFERENCES catalog.execution_bundles (tenant_id, execution_bundle_hash)
);
-- At-least-once: a redelivered trigger with the same key collapses to one run.
CREATE UNIQUE INDEX runs_idempotency ON wamn_run.runs (tenant_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
-- History listing and trusted CDC event-causation traversal.
CREATE INDEX runs_flow ON wamn_run.runs (tenant_id, flow_id, created_at);
CREATE INDEX runs_release ON wamn_run.runs (tenant_id, catalog_id, catalog_version);
CREATE INDEX runs_execution_bundle ON wamn_run.runs (tenant_id, execution_bundle_hash);
CREATE INDEX runs_event_root ON wamn_run.runs (tenant_id, event_root_run_id)
    WHERE event_root_run_id IS NOT NULL;
CREATE INDEX runs_response_deadline ON wamn_run.runs (tenant_id, response_deadline_at)
    WHERE caller_released_at IS NULL
      AND response_deadline_at IS NOT NULL
      AND status IN ('dispatched', 'running');
CREATE INDEX runs_run_deadline ON wamn_run.runs (tenant_id, run_deadline_at)
    WHERE run_deadline_at IS NOT NULL
      AND status IN ('dispatched', 'running');
ALTER TABLE wamn_run.runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.runs FORCE ROW LEVEL SECURITY;
CREATE POLICY runs_tenant ON wamn_run.runs
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));

CREATE TRIGGER runs_event_lineage_immutable
BEFORE UPDATE OF event_source_run_id, event_root_run_id, event_depth
ON wamn_run.runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_event_lineage_immutable();
CREATE TRIGGER runs_admission_pins_immutable
BEFORE UPDATE OF catalog_id, catalog_version, environment, execution_bundle_hash, capture_mode
ON wamn_run.runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_run_admission_pins_immutable();
-- The guest-visible application role may drive the existing run-state columns,
-- but it cannot author or mutate the admission-owned capture carrier. Off-path
-- admissions omit that column and take its fail-closed database default.
REVOKE ALL PRIVILEGES ON TABLE wamn_run.runs FROM PUBLIC, wamn_effect_writer;
GRANT SELECT, DELETE ON wamn_run.runs TO wamn_app;
GRANT INSERT (
    tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version,
    environment, execution_bundle_hash, attachment_id, registration_id,
    event_source_run_id, event_root_run_id, event_depth, status, trigger_source,
    input_json, result_json, state_json, invocation_context,
    admission_context_version, platform_revision, idempotency_key,
    caller_outcome_kind, caller_outcome_json,
    caller_http_status, caller_release_node_id, caller_outcome_hash,
    caller_released_at, response_deadline_at, run_deadline_at, terminal_reason,
    fail_kind, fail_node, fail_reason, created_at, updated_at
), UPDATE (
    tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version,
    environment, execution_bundle_hash, attachment_id, registration_id,
    event_source_run_id, event_root_run_id, event_depth, status, trigger_source,
    input_json, result_json, state_json, invocation_context,
    admission_context_version, platform_revision, idempotency_key,
    caller_outcome_kind, caller_outcome_json,
    caller_http_status, caller_release_node_id, caller_outcome_hash,
    caller_released_at, response_deadline_at, run_deadline_at, terminal_reason,
    fail_kind, fail_node, fail_reason, created_at, updated_at
) ON wamn_run.runs TO wamn_app;
GRANT SELECT ON wamn_run.runs TO wamn_scenario_author;
-- The private effect writer may only recheck that the fenced run still has
-- runnable state. Lease-generation authority remains outside this schema lane.
GRANT SELECT (tenant_id, run_id, status)
    ON wamn_run.runs TO wamn_effect_writer;

-- ---------------------------------------------------------------------------
-- run_flow_resolutions: the immutable release-bound executable map for one run.
-- Claim-time resolution inserts the complete reachable flow-name set exactly
-- once, before guest execution. Retries verify an identical complete map; they
-- never delete, recompute, or rewrite rows. Production claim composition,
-- queue mutation, terminalization, and transaction boundaries live outside this
-- substrate. These evidence rows intentionally do not reference runs: terminal
-- run pruning must not be blocked by immutable resolution evidence, while
-- materialization itself validates that a live run exists under tenant RLS.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.run_flow_resolutions (
    tenant_id             text NOT NULL CHECK (tenant_id <> ''),
    run_id                text NOT NULL CHECK (run_id <> ''),
    flow_id               text NOT NULL CHECK (flow_id <> ''),
    execution_bundle_hash text NOT NULL
        CHECK (execution_bundle_hash ~ '^sha256:[0-9a-f]{64}$'),
    source_artifact_hash  text NOT NULL CHECK (source_artifact_hash <> ''),
    PRIMARY KEY (tenant_id, run_id, flow_id),
    CONSTRAINT run_flow_resolutions_execution_bundle_fk
        FOREIGN KEY (tenant_id, execution_bundle_hash)
        REFERENCES catalog.execution_bundles (tenant_id, execution_bundle_hash)
);
CREATE INDEX run_flow_resolutions_execution_bundle
    ON wamn_run.run_flow_resolutions (tenant_id, execution_bundle_hash);
ALTER TABLE wamn_run.run_flow_resolutions ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.run_flow_resolutions FORCE ROW LEVEL SECURITY;
CREATE POLICY run_flow_resolutions_tenant ON wamn_run.run_flow_resolutions
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
CREATE TRIGGER run_flow_resolutions_update_immutable
BEFORE UPDATE ON wamn_run.run_flow_resolutions
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_flow_resolution_change();
CREATE TRIGGER run_flow_resolutions_delete_immutable
BEFORE DELETE ON wamn_run.run_flow_resolutions
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_flow_resolution_change();
GRANT SELECT, INSERT ON wamn_run.run_flow_resolutions TO wamn_app;
GRANT SELECT ON wamn_run.run_flow_resolutions TO wamn_scenario_author;

CREATE FUNCTION wamn_run.materialize_run_flow_resolutions(
    p_run_id text,
    p_resolution_map jsonb
)
RETURNS TABLE (result_code text, fail_kind text)
LANGUAGE plpgsql
AS $$
DECLARE
    current_tenant text := NULLIF(current_setting('app.tenant', true), '');
    proposed_count int;
    existing_count int;
    inserted_count int := 0;
    root_flow text;
    differs boolean;
BEGIN
    IF jsonb_typeof(p_resolution_map) IS DISTINCT FROM 'array' THEN
        RETURN QUERY SELECT 'refused'::text, 'incompatible-contract'::text;
        RETURN;
    END IF;

    SELECT r.flow_id INTO root_flow
    FROM wamn_run.runs AS r
    WHERE r.tenant_id = current_tenant
      AND r.run_id = p_run_id
    FOR KEY SHARE OF r;
    IF root_flow IS NULL THEN
        RETURN QUERY SELECT 'refused'::text, 'unresolvable-name'::text;
        RETURN;
    END IF;

    DROP TABLE IF EXISTS pg_temp.proposed_run_flow_resolutions;
    CREATE TEMP TABLE proposed_run_flow_resolutions
        ON COMMIT DROP
    AS
    SELECT entry.value ->> 'flow-id' AS flow_id,
           entry.value ->> 'execution-bundle-hash' AS execution_bundle_hash,
           entry.value ->> 'source-artifact-hash' AS source_artifact_hash
    FROM jsonb_array_elements(p_resolution_map) AS entry(value);

    SELECT count(*) INTO proposed_count FROM pg_temp.proposed_run_flow_resolutions;
    IF proposed_count = 0
       OR EXISTS (
            SELECT 1 FROM pg_temp.proposed_run_flow_resolutions AS proposed
            WHERE proposed.flow_id IS NULL OR proposed.flow_id = ''
               OR proposed.execution_bundle_hash IS NULL
               OR proposed.execution_bundle_hash !~ '^sha256:[0-9a-f]{64}$'
               OR proposed.source_artifact_hash IS NULL
               OR proposed.source_artifact_hash = ''
       )
       OR EXISTS (
            SELECT 1
            FROM pg_temp.proposed_run_flow_resolutions AS proposed
            GROUP BY proposed.flow_id
            HAVING count(*) > 1
       ) THEN
        RETURN QUERY SELECT 'refused'::text, 'incompatible-contract'::text;
        RETURN;
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_temp.proposed_run_flow_resolutions
        WHERE flow_id = root_flow
    ) THEN
        RETURN QUERY SELECT 'refused'::text, 'unresolvable-name'::text;
        RETURN;
    END IF;

    SELECT count(*) INTO existing_count
    FROM wamn_run.run_flow_resolutions AS existing
    WHERE existing.tenant_id = current_tenant
      AND existing.run_id = p_run_id;

    -- The inner block is a savepoint: a mismatched retry rolls back any rows
    -- it inserted before returning the stable foreign-revision refusal.
    BEGIN
        -- Any pre-existing row makes this a verification-only retry. Two true
        -- first claims can both observe zero, so ordered key conflicts choose
        -- one complete-map winner without deadlocking on shared flow ids.
        IF existing_count = 0 THEN
            INSERT INTO wamn_run.run_flow_resolutions (
                tenant_id, run_id, flow_id, execution_bundle_hash, source_artifact_hash
            )
            SELECT current_tenant, p_run_id, proposed.flow_id,
                   proposed.execution_bundle_hash, proposed.source_artifact_hash
            FROM pg_temp.proposed_run_flow_resolutions AS proposed
            ORDER BY proposed.flow_id
            ON CONFLICT (tenant_id, run_id, flow_id) DO NOTHING;
            GET DIAGNOSTICS inserted_count = ROW_COUNT;
        END IF;

        SELECT EXISTS (
            (
                SELECT existing.flow_id, existing.execution_bundle_hash,
                       existing.source_artifact_hash
                FROM wamn_run.run_flow_resolutions AS existing
                WHERE existing.tenant_id = current_tenant
                  AND existing.run_id = p_run_id
                EXCEPT
                SELECT proposed.flow_id, proposed.execution_bundle_hash,
                       proposed.source_artifact_hash
                FROM pg_temp.proposed_run_flow_resolutions AS proposed
            )
            UNION ALL
            (
                SELECT proposed.flow_id, proposed.execution_bundle_hash,
                       proposed.source_artifact_hash
                FROM pg_temp.proposed_run_flow_resolutions AS proposed
                EXCEPT
                SELECT existing.flow_id, existing.execution_bundle_hash,
                       existing.source_artifact_hash
                FROM wamn_run.run_flow_resolutions AS existing
                WHERE existing.tenant_id = current_tenant
                  AND existing.run_id = p_run_id
            )
        ) INTO differs;
        IF differs
           OR (existing_count = 0
               AND inserted_count NOT IN (0, proposed_count)) THEN
            -- P0004 is private to this block and means exact-map verification
            -- failed; constraint failures retain their existing mapping below.
            RAISE EXCEPTION USING
                ERRCODE = 'P0004',
                MESSAGE = 'run-flow-resolution-map-mismatch';
        END IF;
    EXCEPTION
        WHEN SQLSTATE 'P0004' THEN
            RETURN QUERY SELECT 'refused'::text, 'foreign-revision'::text;
            RETURN;
    END;

    RETURN QUERY SELECT 'resolved'::text, NULL::text;
EXCEPTION
    WHEN foreign_key_violation OR check_violation OR unique_violation THEN
        RETURN QUERY SELECT 'refused'::text, 'foreign-revision'::text;
END
$$;
REVOKE ALL ON FUNCTION wamn_run.materialize_run_flow_resolutions(text, jsonb)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION wamn_run.materialize_run_flow_resolutions(text, jsonb)
    TO wamn_app;

-- HTTP invocation idempotency ledger (§6.2). The identity is intentionally
-- definition-independent: reusing a client key after a definition change must
-- find the old admission and return `idempotency-scope-changed`, never create a
-- second run. The named unique constraint is mapped to the transitions module's
-- typed `duplicate-identity` refusal. Pure call-free admissions may omit a
-- client key; PostgreSQL's distinct NULLs make each such admission a new run.
CREATE TABLE wamn_run.invocation_admissions (
    tenant_id                  text NOT NULL CHECK (tenant_id <> ''),
    catalog_id                 text NOT NULL,
    environment                text NOT NULL,
    attachment_id              text NOT NULL,
    definition_hash            text NOT NULL,
    principal_digest           text NOT NULL,
    client_key_digest          text,
    client_request_fingerprint text NOT NULL,
    admitted_catalog_version   bigint NOT NULL,
    admitted_flow_version      int NOT NULL,
    run_id                     text NOT NULL,
    created_at                 timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT invocation_admissions_identity UNIQUE
        (tenant_id, catalog_id, environment, attachment_id,
         principal_digest, client_key_digest),
    CONSTRAINT invocation_admissions_run_fk FOREIGN KEY (tenant_id, run_id)
        REFERENCES wamn_run.runs (tenant_id, run_id) ON DELETE CASCADE
        DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX invocation_admissions_run
    ON wamn_run.invocation_admissions (tenant_id, run_id);
ALTER TABLE wamn_run.invocation_admissions ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.invocation_admissions FORCE ROW LEVEL SECURITY;
CREATE POLICY invocation_admissions_tenant ON wamn_run.invocation_admissions
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.invocation_admissions TO wamn_app;

-- ---------------------------------------------------------------------------
-- node_runs: one row per framed node execution.
-- The occurrence identity is (tenant_id, run_id, frame_id, local_node_id, occurrence):
-- `occurrence` disambiguates a node the frame LOOPS through (0 = first visit).
-- Frameless/call-free executions use the root frame (frame_id 0, no parent or
-- call site) with current_plan_hash = runs.execution_bundle_hash.
-- `seq` preserves execution order for history reads. `input_json` and
-- `output_json` carry capture facts when the run's admission mode allows them.
-- The `*_ref` columns are RESERVED nullable seams
-- for 5.10 (payload byte store). Capture policy belongs to the run admission
-- row and is not duplicated per node; full capture stores only scrubbed input,
-- output, output size, and the optional output hash.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.node_runs (
    tenant_id     text NOT NULL CHECK (tenant_id <> ''),
    run_id        text NOT NULL,
    frame_id      bigint NOT NULL DEFAULT 0,
    parent_frame_id bigint,
    call_site_id  text,
    current_plan_hash text NOT NULL,
    local_node_id text NOT NULL,
    occurrence    int  NOT NULL DEFAULT 0,
    seq           bigint NOT NULL,
    status        text NOT NULL
        CHECK (status IN ('started', 'success', 'error')),
    output_port   text,
    output_json   jsonb,
    input_json    jsonb,
    error_kind    text CHECK (error_kind IN ('retryable', 'rate-limited', 'terminal',
                                            'invalid-input')),
    error_detail  jsonb,
    -- Reserved seams (5.10 payload byte store / 9.6 capture policy):
    input_ref     text,
    output_ref    text,
    output_size   bigint,
    payload_hash  text,
    started_at    timestamptz NOT NULL DEFAULT now(),
    ended_at      timestamptz,
    CONSTRAINT node_runs_frame_check CHECK (frame_id >= 0),
    CONSTRAINT node_runs_frame_relation_check CHECK (
        (frame_id = 0 AND parent_frame_id IS NULL AND call_site_id IS NULL)
        OR
        (frame_id > 0 AND parent_frame_id IS NOT NULL AND parent_frame_id >= 0
         AND parent_frame_id < frame_id AND call_site_id IS NOT NULL
         AND call_site_id ~ '^[a-z0-9-]+$')
    ),
    CONSTRAINT node_runs_plan_hash_check
        CHECK (current_plan_hash ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT node_runs_local_node_check CHECK (local_node_id ~ '^[a-z0-9-]+$'),
    PRIMARY KEY (tenant_id, run_id, frame_id, local_node_id, occurrence),
    FOREIGN KEY (tenant_id, run_id) REFERENCES wamn_run.runs (tenant_id, run_id) ON DELETE CASCADE
);
-- Run history reads rows in dispatch order.
CREATE INDEX node_runs_seq ON wamn_run.node_runs (tenant_id, run_id, seq);
ALTER TABLE wamn_run.node_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.node_runs FORCE ROW LEVEL SECURITY;
CREATE POLICY node_runs_tenant ON wamn_run.node_runs
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON wamn_run.node_runs TO wamn_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.node_runs
    TO wamn_run_projection_writer;

-- ---------------------------------------------------------------------------
-- Immutable effect-attempt ledger. `node_runs` remains the current occurrence
-- projection; every effectful occurrence has one server-minted identity here.
-- wamn-0h0g.4.9 installs the inaccessible writer primitive. wamn-0h0g.5.4
-- first wires and activates it; until then execution remains hard-refused.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.effect_attempts (
    tenant_id       text NOT NULL,
    attempt_id      uuid NOT NULL DEFAULT gen_random_uuid(),
    run_id          text NOT NULL,
    root_plan_hash  text NOT NULL,
    current_plan_hash text NOT NULL,
    frame_id        bigint NOT NULL DEFAULT 0,
    parent_frame_id bigint,
    call_site_id    text,
    local_node_id   text NOT NULL,
    source_artifact_hash text NOT NULL,
    requirement_name text NOT NULL,
    occurrence      int NOT NULL,
    seq             int NOT NULL,
    generation_fact_kind text NOT NULL,
    connection_name text,
    connection_generation text,
    credential_generation text,
    verified_author_principal text,
    verified_publisher_principal text,
    attempt_started_at timestamptz NOT NULL DEFAULT now(),
    attempt_deadline_at timestamptz NOT NULL,
    attempt_input_ref text NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT effect_attempts_tenant_check CHECK (tenant_id <> ''),
    CONSTRAINT effect_attempts_root_plan_hash_check
        CHECK (root_plan_hash ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT effect_attempts_current_plan_hash_check
        CHECK (current_plan_hash ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT effect_attempts_frame_check CHECK (frame_id >= 0),
    CONSTRAINT effect_attempts_frame_relation_check CHECK (
        (frame_id = 0 AND parent_frame_id IS NULL AND call_site_id IS NULL)
        OR
        (frame_id > 0 AND parent_frame_id IS NOT NULL AND parent_frame_id >= 0
         AND parent_frame_id < frame_id AND call_site_id IS NOT NULL
         AND call_site_id ~ '^[a-z0-9-]+$')
    ),
    CONSTRAINT effect_attempts_local_node_check CHECK (local_node_id ~ '^[a-z0-9-]+$'),
    CONSTRAINT effect_attempts_source_artifact_check
        CHECK (source_artifact_hash ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT effect_attempts_requirement_check CHECK (requirement_name <> ''),
    CONSTRAINT effect_attempts_occurrence_check CHECK (occurrence >= 0),
    CONSTRAINT effect_attempts_seq_check CHECK (seq >= 0),
    CONSTRAINT effect_attempts_generation_fact_check
        CHECK (generation_fact_kind IN ('not-required', 'attested')),
    CONSTRAINT effect_attempts_generation_values_check CHECK (
        (generation_fact_kind = 'not-required'
         AND connection_name IS NULL
         AND connection_generation IS NULL AND credential_generation IS NULL)
        OR
        (generation_fact_kind = 'attested'
         AND connection_name IS NOT NULL AND connection_name <> ''
         AND connection_generation IS NOT NULL AND connection_generation <> ''
         AND credential_generation IS NOT NULL AND credential_generation <> '')
    ),
    CONSTRAINT effect_attempts_author_check
        CHECK (verified_author_principal IS NULL OR verified_author_principal <> ''),
    CONSTRAINT effect_attempts_publisher_check
        CHECK (verified_publisher_principal IS NULL OR verified_publisher_principal <> ''),
    CONSTRAINT effect_attempts_deadline_check
        CHECK (attempt_started_at <= attempt_deadline_at),
    CONSTRAINT effect_attempts_input_ref_check
        CHECK (attempt_input_ref <> ''),
    PRIMARY KEY (tenant_id, attempt_id),
    CONSTRAINT effect_attempts_occurrence_key
        UNIQUE (tenant_id, run_id, frame_id, local_node_id, occurrence),
    CONSTRAINT effect_attempts_dispatch_identity_key
        UNIQUE (tenant_id, attempt_id, attempt_started_at,
                run_id, frame_id, local_node_id, occurrence)
);
-- Deliberately no FK from (tenant_id, run_id) to runs: effect attempts are an
-- audit ledger with an independent retention lifetime. Pruning terminal run
-- history cascades through the mutable node projection but must leave these
-- immutable facts intact.
CREATE INDEX effect_attempts_bulk_scope
    ON wamn_run.effect_attempts
       (tenant_id, connection_name, connection_generation, attempt_started_at);
ALTER TABLE wamn_run.effect_attempts ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.effect_attempts FORCE ROW LEVEL SECURITY;
CREATE POLICY effect_attempts_tenant ON wamn_run.effect_attempts
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
REVOKE ALL PRIVILEGES ON TABLE wamn_run.effect_attempts
    FROM PUBLIC, wamn_app, wamn_scenario_author, wamn_effect_writer;
GRANT SELECT ON wamn_run.effect_attempts TO wamn_app;
GRANT SELECT, INSERT ON wamn_run.effect_attempts TO wamn_effect_writer;
CREATE TRIGGER effect_attempts_update_immutable
BEFORE UPDATE ON wamn_run.effect_attempts
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_effect_fact_change();
CREATE TRIGGER effect_attempts_delete_immutable
BEFORE DELETE ON wamn_run.effect_attempts
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_effect_fact_change();

CREATE TABLE wamn_run.effect_attempt_dispatches (
    tenant_id       text NOT NULL,
    attempt_id      uuid NOT NULL,
    attempt_started_at timestamptz NOT NULL,
    run_id          text NOT NULL,
    frame_id        bigint NOT NULL,
    local_node_id   text NOT NULL,
    occurrence      int NOT NULL,
    dispatched_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT effect_attempt_dispatches_tenant_check CHECK (tenant_id <> ''),
    CONSTRAINT effect_attempt_dispatches_frame_check CHECK (frame_id >= 0),
    CONSTRAINT effect_attempt_dispatches_local_node_check
        CHECK (local_node_id ~ '^[a-z0-9-]+$'),
    CONSTRAINT effect_attempt_dispatches_occurrence_check CHECK (occurrence >= 0),
    CONSTRAINT effect_attempt_dispatches_time_check
        CHECK (attempt_started_at <= dispatched_at),
    PRIMARY KEY (tenant_id, attempt_id),
    UNIQUE (tenant_id, attempt_id, dispatched_at),
    CONSTRAINT effect_attempt_dispatches_occurrence_key
        UNIQUE (tenant_id, run_id, frame_id, local_node_id, occurrence),
    CONSTRAINT effect_attempt_dispatches_attempt_fk
        FOREIGN KEY (tenant_id, attempt_id, attempt_started_at,
                     run_id, frame_id, local_node_id, occurrence)
        REFERENCES wamn_run.effect_attempts
            (tenant_id, attempt_id, attempt_started_at,
             run_id, frame_id, local_node_id, occurrence)
);
ALTER TABLE wamn_run.effect_attempt_dispatches ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.effect_attempt_dispatches FORCE ROW LEVEL SECURITY;
CREATE POLICY effect_attempt_dispatches_tenant ON wamn_run.effect_attempt_dispatches
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
REVOKE ALL PRIVILEGES ON TABLE wamn_run.effect_attempt_dispatches
    FROM PUBLIC, wamn_app, wamn_scenario_author, wamn_effect_writer;
GRANT SELECT ON wamn_run.effect_attempt_dispatches TO wamn_app;
GRANT SELECT, INSERT ON wamn_run.effect_attempt_dispatches TO wamn_effect_writer;
CREATE TRIGGER effect_attempt_dispatches_update_immutable
BEFORE UPDATE ON wamn_run.effect_attempt_dispatches
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_effect_fact_change();
CREATE TRIGGER effect_attempt_dispatches_delete_immutable
BEFORE DELETE ON wamn_run.effect_attempt_dispatches
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_effect_fact_change();

CREATE TABLE wamn_run.effect_attempt_outcomes (
    tenant_id       text NOT NULL,
    attempt_id      uuid NOT NULL,
    dispatched_at   timestamptz NOT NULL,
    outcome_status  text NOT NULL,
    recorded_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT effect_attempt_outcomes_tenant_check CHECK (tenant_id <> ''),
    CONSTRAINT effect_attempt_outcomes_status_check
        CHECK (outcome_status IN ('success', 'error')),
    CONSTRAINT effect_attempt_outcomes_time_check
        CHECK (dispatched_at <= recorded_at),
    PRIMARY KEY (tenant_id, attempt_id),
    CONSTRAINT effect_attempt_outcomes_dispatch_fk
        FOREIGN KEY (tenant_id, attempt_id, dispatched_at)
        REFERENCES wamn_run.effect_attempt_dispatches
            (tenant_id, attempt_id, dispatched_at)
);
ALTER TABLE wamn_run.effect_attempt_outcomes ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.effect_attempt_outcomes FORCE ROW LEVEL SECURITY;
CREATE POLICY effect_attempt_outcomes_tenant ON wamn_run.effect_attempt_outcomes
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
REVOKE ALL PRIVILEGES ON TABLE wamn_run.effect_attempt_outcomes
    FROM PUBLIC, wamn_app, wamn_scenario_author, wamn_effect_writer;
GRANT SELECT ON wamn_run.effect_attempt_outcomes TO wamn_app;
GRANT SELECT, INSERT ON wamn_run.effect_attempt_outcomes TO wamn_effect_writer;
CREATE TRIGGER effect_attempt_outcomes_update_immutable
BEFORE UPDATE ON wamn_run.effect_attempt_outcomes
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_effect_fact_change();
CREATE TRIGGER effect_attempt_outcomes_delete_immutable
BEFORE DELETE ON wamn_run.effect_attempt_outcomes
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_effect_fact_change();

-- ---------------------------------------------------------------------------
-- Immutable operator resolution evidence. There is deliberately no FK to run
-- or node history: terminal history can be pruned without erasing the action.
-- The project-admin transaction supplies SESSION_USER as database-role
-- attribution and appends before changing the mutable terminal projections.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.operator_run_actions (
    tenant_id       text NOT NULL,
    action_id       uuid NOT NULL DEFAULT gen_random_uuid(),
    correlation_id  text NOT NULL,
    run_id          text NOT NULL,
    action_kind     text NOT NULL,
    basis           text NOT NULL,
    evidence_ref    text NOT NULL,
    principal       text NOT NULL,
    principal_kind  text NOT NULL,
    prior_run_status text NOT NULL,
    prior_started_node_frame_id bigint,
    prior_started_node_local_node_id text,
    prior_started_node_occurrence int,
    prior_started_node_status text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT operator_run_actions_tenant_check CHECK (tenant_id <> ''),
    CONSTRAINT operator_run_actions_correlation_check CHECK (correlation_id <> ''),
    CONSTRAINT operator_run_actions_run_check CHECK (run_id <> ''),
    CONSTRAINT operator_run_actions_kind_check
        CHECK (action_kind = 'terminalize-effect-uncertain'),
    CONSTRAINT operator_run_actions_basis_check
        CHECK (basis IN ('external-evidence', 'counterparty-confirmation',
                         'operator-judgment')),
    CONSTRAINT operator_run_actions_evidence_check CHECK (evidence_ref <> ''),
    CONSTRAINT operator_run_actions_principal_check CHECK (principal <> ''),
    CONSTRAINT operator_run_actions_principal_kind_check
        CHECK (principal_kind = 'database-role'),
    CONSTRAINT operator_run_actions_prior_run_status_check
        CHECK (prior_run_status = 'effect-uncertain'),
    CONSTRAINT operator_run_actions_prior_node_check CHECK (
        (prior_started_node_frame_id IS NULL
         AND prior_started_node_local_node_id IS NULL
         AND prior_started_node_occurrence IS NULL
         AND prior_started_node_status IS NULL)
        OR
        (prior_started_node_frame_id >= 0
         AND prior_started_node_local_node_id IS NOT NULL
         AND prior_started_node_local_node_id ~ '^[a-z0-9-]+$'
         AND prior_started_node_occurrence >= 0
         AND prior_started_node_status = 'started')
    ),
    PRIMARY KEY (tenant_id, action_id),
    CONSTRAINT operator_run_actions_run_key UNIQUE (tenant_id, run_id),
    CONSTRAINT operator_run_actions_correlation_key UNIQUE (tenant_id, correlation_id)
);
ALTER TABLE wamn_run.operator_run_actions ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.operator_run_actions FORCE ROW LEVEL SECURITY;
CREATE POLICY operator_run_actions_tenant ON wamn_run.operator_run_actions
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
REVOKE ALL PRIVILEGES ON TABLE wamn_run.operator_run_actions
    FROM PUBLIC, wamn_app, wamn_scenario_author, wamn_effect_writer;
CREATE TRIGGER operator_run_actions_update_immutable
BEFORE UPDATE ON wamn_run.operator_run_actions
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_operator_run_action_change();
CREATE TRIGGER operator_run_actions_delete_immutable
BEFORE DELETE ON wamn_run.operator_run_actions
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_operator_run_action_change();
