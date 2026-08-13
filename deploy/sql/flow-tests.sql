-- Flow test-suite storage (11.2). A flow's test cases live as CATALOG DATA,
-- versioned WITH the flow they test: every suite/case row pins a concrete
-- `(flow_id, flow_version)`, and the FK to `wamn_run.flows(tenant_id, flow_id,
-- version)` ON DELETE CASCADE makes that binding structural — dropping a flow
-- version takes its suites (and their cases) with it, and a flow version and
-- its suite promote together through the copy-project-env definition path
-- (services/ctl copy_project_env: flows in block 2, suites in block 5).
--
-- The case BODY is opaque jsonb in v0 (`case_body`). The product worker decodes
-- that persisted envelope through its private `LegacyStoredTestCase` bridge and
-- validates only the MVP assertion vocabulary; the public inline test-set parser
-- never accepts this envelope. `flow_version` is DENORMALIZED onto `test_cases`
-- (not reachable only through `test_suites`) — the event_registrations
-- precedent (deploy/sql/catalog-schema.sql): it is part of the composite FK to
-- the suite and lets the promote-copy scope cases by version without a join.
--
-- STANDALONE ARTIFACT, ADDITIVE to deploy/sql/flows.sql: same convention as
-- flows.sql / run-queue.sql — deliberately NOT included by
-- deploy/sql/postgres-init.sql. Assumes deploy/sql/flows.sql has been applied
-- first (the `flows` table this FKs, the `wamn_run` schema, the guest-visible
-- `wamn_app` role, and the host-only `wamn_scenario_author` NOLOGIN role).
-- Provisioning a per-project schema rewrites `wamn_run` to the project
-- schema (`wamn-ctl publish-catalog --runstate`, reconcile-run-plane).
--
-- Security shape mirrors flows.sql: FORCE RLS keyed on
-- NULLIF(current_setting('app.tenant', true), ''); an empty/absent claim reads
-- as NULL => zero rows, and CHECK (tenant_id <> '') forbids a ''-tenant row.

CREATE TABLE wamn_run.test_suites (
    tenant_id    text NOT NULL CHECK (tenant_id <> ''),
    flow_id      text NOT NULL,
    flow_version int  NOT NULL,
    suite_id     text NOT NULL,
    name         text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, flow_id, flow_version, suite_id),
    FOREIGN KEY (tenant_id, flow_id, flow_version)
        REFERENCES wamn_run.flows (tenant_id, flow_id, version) ON DELETE CASCADE
);
ALTER TABLE wamn_run.test_suites ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.test_suites FORCE ROW LEVEL SECURITY;
CREATE POLICY test_suites_tenant ON wamn_run.test_suites
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.test_suites TO wamn_app;
GRANT SELECT ON wamn_run.test_suites TO wamn_scenario_author;

CREATE TABLE wamn_run.test_cases (
    tenant_id    text NOT NULL CHECK (tenant_id <> ''),
    flow_id      text NOT NULL,
    flow_version int  NOT NULL,
    suite_id     text NOT NULL,
    case_id      text NOT NULL,
    ordinal      int  NOT NULL,
    case_body    jsonb NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, flow_id, flow_version, suite_id, case_id),
    FOREIGN KEY (tenant_id, flow_id, flow_version, suite_id)
        REFERENCES wamn_run.test_suites (tenant_id, flow_id, flow_version, suite_id) ON DELETE CASCADE
);
ALTER TABLE wamn_run.test_cases ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.test_cases FORCE ROW LEVEL SECURITY;
CREATE POLICY test_cases_tenant ON wamn_run.test_cases
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.test_cases TO wamn_app;
GRANT SELECT ON wamn_run.test_cases TO wamn_scenario_author;

-- Inline test-set definitions are management-owned, content-addressed inputs.
-- The exact submitted UTF-8 bytes are retained once; the document carries no
-- draft selector because the `test-set-run` operation owns that selection.
CREATE OR REPLACE FUNCTION wamn_run.reject_immutable_authoring_test_set_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = 'authoring-test-set-immutable';
END
$$;
REVOKE ALL ON FUNCTION wamn_run.reject_immutable_authoring_test_set_change() FROM PUBLIC;

CREATE TABLE wamn_run.authoring_test_sets (
    tenant_id      text NOT NULL CHECK (tenant_id <> ''),
    test_set_hash  text NOT NULL,
    schema_version text NOT NULL CHECK (schema_version = '0.1'),
    exact_bytes    bytea NOT NULL,
    byte_length    int NOT NULL CHECK (byte_length BETWEEN 1 AND 1048576),
    created_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, test_set_hash),
    CHECK (byte_length = octet_length(exact_bytes)),
    CHECK (test_set_hash = 'sha256:' || encode(sha256(exact_bytes), 'hex')),
    CHECK ((convert_from(exact_bytes, 'UTF8')::jsonb ->> 'schema-version')
           IS NOT DISTINCT FROM schema_version)
);
ALTER TABLE wamn_run.authoring_test_sets ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.authoring_test_sets FORCE ROW LEVEL SECURITY;
CREATE POLICY authoring_test_sets_tenant ON wamn_run.authoring_test_sets
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
CREATE TRIGGER authoring_test_sets_update_immutable
BEFORE UPDATE ON wamn_run.authoring_test_sets
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_authoring_test_set_change();
CREATE TRIGGER authoring_test_sets_delete_immutable
BEFORE DELETE ON wamn_run.authoring_test_sets
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_authoring_test_set_change();
GRANT SELECT, INSERT ON wamn_run.authoring_test_sets TO wamn_scenario_author;

-- Durable inline-test orchestration. This replacement is intentionally
-- separate from the retained stored-suite compatibility tables below: the
-- later deletion/fold beads remove that legacy lifecycle without renaming it.
CREATE OR REPLACE FUNCTION wamn_run.reject_immutable_authoring_test_orchestration_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = 'authoring-test-orchestration-immutable';
END
$$;
REVOKE ALL ON FUNCTION wamn_run.reject_immutable_authoring_test_orchestration_change()
    FROM PUBLIC;

CREATE TABLE wamn_run.authoring_test_run_reservations (
    tenant_id          text NOT NULL CHECK (tenant_id <> ''),
    report_id          text NOT NULL CHECK (report_id <> ''),
    command_hash       text NOT NULL
        CHECK (command_hash ~ '^sha256:[0-9a-f]{64}$'),
    validated_draft_id text NOT NULL CHECK (validated_draft_id <> ''),
    test_set_hash      text NOT NULL,
    catalog_id         text NOT NULL CHECK (catalog_id <> ''),
    catalog_version    int NOT NULL CHECK (catalog_version > 0),
    case_count         int NOT NULL CHECK (case_count BETWEEN 1 AND 256),
    resolution_map     jsonb CHECK (
        resolution_map IS NULL OR jsonb_typeof(resolution_map) = 'object'
    ),
    resolution_map_hash text CHECK (
        resolution_map_hash IS NULL
        OR resolution_map_hash ~ '^sha256:[0-9a-f]{64}$'
    ),
    state              text NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'finalized')),
    created_at         timestamptz NOT NULL DEFAULT clock_timestamp(),
    whole_deadline_at  timestamptz NOT NULL,
    finalized_at       timestamptz,
    PRIMARY KEY (tenant_id, report_id),
    UNIQUE (
        tenant_id, report_id, catalog_id, catalog_version,
        validated_draft_id
    ),
    CONSTRAINT authoring_test_reservation_test_set_fk
        FOREIGN KEY (tenant_id, test_set_hash)
        REFERENCES wamn_run.authoring_test_sets (tenant_id, test_set_hash),
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
ALTER TABLE wamn_run.authoring_test_run_reservations ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.authoring_test_run_reservations FORCE ROW LEVEL SECURITY;
CREATE POLICY authoring_test_run_reservations_tenant
    ON wamn_run.authoring_test_run_reservations
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE ON wamn_run.authoring_test_run_reservations
    TO wamn_scenario_author;

-- Every case gets its deterministic run identity before admission. The row is
-- then append-like: only its pending -> finalized result transition is allowed.
CREATE TABLE wamn_run.authoring_test_case_runs (
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
                         'effect-uncertain', 'resolution-map-mismatch')
    ),
    resolution_map     jsonb CHECK (
        resolution_map IS NULL OR jsonb_typeof(resolution_map) = 'object'
    ),
    resolution_map_hash text CHECK (
        resolution_map_hash IS NULL
        OR resolution_map_hash ~ '^sha256:[0-9a-f]{64}$'
    ),
    summary             jsonb CHECK (
        summary IS NULL OR jsonb_typeof(summary) = 'object'
    ),
    case_deadline_at   timestamptz NOT NULL,
    created_at         timestamptz NOT NULL DEFAULT clock_timestamp(),
    finalized_at       timestamptz,
    PRIMARY KEY (tenant_id, report_id, ordinal),
    UNIQUE (tenant_id, report_id, case_id),
    UNIQUE (tenant_id, run_id),
    CONSTRAINT authoring_test_case_reservation_fk FOREIGN KEY (
        tenant_id, report_id, catalog_id, catalog_version,
        validated_draft_id
    ) REFERENCES wamn_run.authoring_test_run_reservations (
        tenant_id, report_id, catalog_id, catalog_version,
        validated_draft_id
    ),
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
ALTER TABLE wamn_run.authoring_test_case_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.authoring_test_case_runs FORCE ROW LEVEL SECURITY;
CREATE POLICY authoring_test_case_runs_tenant ON wamn_run.authoring_test_case_runs
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE ON wamn_run.authoring_test_case_runs
    TO wamn_scenario_author;

-- Final reports copy every publication-relevant pin. The map is the exact
-- flow_id -> execution_bundle_hash object consumed later as tested_resolution_map.
CREATE TABLE wamn_run.authoring_test_reports (
    tenant_id          text NOT NULL CHECK (tenant_id <> ''),
    report_id          text NOT NULL CHECK (report_id <> ''),
    validated_draft_id text NOT NULL CHECK (validated_draft_id <> ''),
    test_set_hash      text NOT NULL,
    catalog_id         text NOT NULL CHECK (catalog_id <> ''),
    catalog_version    int NOT NULL CHECK (catalog_version > 0),
    resolution_map     jsonb NOT NULL CHECK (jsonb_typeof(resolution_map) = 'object'),
    resolution_map_hash text NOT NULL
        CHECK (resolution_map_hash ~ '^sha256:[0-9a-f]{64}$'),
    passed             boolean NOT NULL,
    summary            jsonb NOT NULL CHECK (jsonb_typeof(summary) = 'object'),
    finalized_at       timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, report_id),
    CONSTRAINT authoring_test_report_reservation_fk
        FOREIGN KEY (tenant_id, report_id)
        REFERENCES wamn_run.authoring_test_run_reservations (tenant_id, report_id),
    CONSTRAINT authoring_test_report_test_set_fk
        FOREIGN KEY (tenant_id, test_set_hash)
        REFERENCES wamn_run.authoring_test_sets (tenant_id, test_set_hash),
    CHECK (
        resolution_map_hash = 'sha256:' || encode(
            sha256(convert_to(resolution_map::text, 'UTF8')), 'hex'
        )
    )
);
ALTER TABLE wamn_run.authoring_test_reports ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.authoring_test_reports FORCE ROW LEVEL SECURITY;
CREATE POLICY authoring_test_reports_tenant ON wamn_run.authoring_test_reports
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT ON wamn_run.authoring_test_reports TO wamn_scenario_author;

CREATE OR REPLACE FUNCTION wamn_run.guard_authoring_test_orchestration_write()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    old_row jsonb := CASE WHEN TG_OP = 'UPDATE' THEN to_jsonb(OLD) END;
    new_row jsonb := to_jsonb(NEW);
    reservation record;
    actual_resolution_map jsonb;
    expected_count bigint;
    finalized_count bigint;
    all_passed boolean;
    every_map_matches boolean;
    expected_summary jsonb;
BEGIN
    IF TG_TABLE_NAME = 'authoring_test_run_reservations' THEN
        IF TG_OP = 'INSERT' THEN
            IF NEW.state <> 'pending' OR NEW.finalized_at IS NOT NULL THEN
                RAISE EXCEPTION USING ERRCODE = '55000',
                    MESSAGE = 'authoring-test-reservation-must-start-pending';
            END IF;
        ELSIF TG_OP = 'UPDATE' THEN
            IF OLD.state = 'pending' AND NEW.state = 'pending'
               AND OLD.resolution_map IS NULL
               AND NEW.resolution_map IS NOT NULL
               AND (new_row - 'resolution_map' - 'resolution_map_hash')
                   = (old_row - 'resolution_map' - 'resolution_map_hash')
               AND EXISTS (
                   SELECT 1
                   FROM wamn_run.authoring_test_case_runs AS test_case
                   JOIN wamn_run.runs AS run
                     ON run.tenant_id = test_case.tenant_id
                    AND run.run_id = test_case.run_id
                   WHERE test_case.tenant_id = NEW.tenant_id
                     AND test_case.report_id = NEW.report_id
                     AND run.catalog_id = NEW.catalog_id
                     AND run.catalog_version = NEW.catalog_version
                     AND run.invocation_context #>>
                         '{principal,validated-draft-hash}' = NEW.validated_draft_id
                     AND run.invocation_context #>>
                         '{source,report-id}' = NEW.report_id
                     AND run.invocation_context #>>
                         '{source,case-id}' = test_case.case_id
                     AND EXISTS (
                         SELECT 1 FROM wamn_run.run_flow_resolutions AS present
                         WHERE present.tenant_id = test_case.tenant_id
                           AND present.run_id = test_case.run_id
                     )
                     AND NEW.resolution_map = (
                         SELECT jsonb_object_agg(
                             map.flow_id, map.execution_bundle_hash
                             ORDER BY map.flow_id
                         )
                         FROM wamn_run.run_flow_resolutions AS map
                         WHERE map.tenant_id = test_case.tenant_id
                           AND map.run_id = test_case.run_id
                     )
               ) THEN
                NULL;
            ELSIF OLD.state = 'pending' AND NEW.state = 'finalized'
               AND (new_row - 'state' - 'finalized_at')
                   = (old_row - 'state' - 'finalized_at')
               AND EXISTS (
                   SELECT 1 FROM wamn_run.authoring_test_reports AS report
                   WHERE report.tenant_id = OLD.tenant_id
                     AND report.report_id = OLD.report_id
               ) THEN
                NULL;
            ELSE
                RAISE EXCEPTION USING ERRCODE = '55000',
                    MESSAGE = 'authoring-test-reservation-uncontrolled-update';
            END IF;
        ELSE
            RAISE EXCEPTION USING ERRCODE = '55000',
                MESSAGE = 'authoring-test-reservation-unexpected-operation';
        END IF;
    ELSIF TG_TABLE_NAME = 'authoring_test_case_runs' THEN
        SELECT * INTO reservation
        FROM wamn_run.authoring_test_run_reservations AS candidate
        WHERE candidate.tenant_id = NEW.tenant_id
          AND candidate.report_id = NEW.report_id
        FOR UPDATE;
        IF NOT FOUND OR reservation.state <> 'pending' THEN
            RAISE EXCEPTION USING ERRCODE = '55000',
                MESSAGE = 'authoring-test-case-requires-pending-report';
        END IF;
        IF TG_OP = 'INSERT' THEN
            IF NEW.state <> 'pending' OR NEW.ordinal >= reservation.case_count
               OR NEW.case_deadline_at > reservation.whole_deadline_at THEN
                RAISE EXCEPTION USING ERRCODE = '23514',
                    MESSAGE = 'authoring-test-case-reservation-invalid';
            END IF;
        ELSIF TG_OP = 'UPDATE' THEN
            IF (new_row - 'state' - 'passed' - 'failure_kind'
                        - 'resolution_map' - 'resolution_map_hash'
                        - 'summary' - 'finalized_at')
                   <> (old_row - 'state' - 'passed' - 'failure_kind'
                        - 'resolution_map' - 'resolution_map_hash'
                        - 'summary' - 'finalized_at')
               OR OLD.state <> 'pending' OR NEW.state <> 'finalized' THEN
                RAISE EXCEPTION USING ERRCODE = '55000',
                    MESSAGE = 'authoring-test-case-uncontrolled-update';
            END IF;
            IF NEW.failure_kind = 'deadline-exhausted'
               AND clock_timestamp() < LEAST(
                   NEW.case_deadline_at, reservation.whole_deadline_at
               ) THEN
                RAISE EXCEPTION USING ERRCODE = '55000',
                    MESSAGE = 'authoring-test-case-deadline-not-exhausted';
            END IF;
            IF COALESCE(NEW.failure_kind, '') NOT IN (
                   'deadline-exhausted', 'effect-uncertain'
               ) AND clock_timestamp() >= LEAST(
                   NEW.case_deadline_at, reservation.whole_deadline_at
               ) THEN
                RAISE EXCEPTION USING ERRCODE = '55000',
                    MESSAGE = 'authoring-test-case-deadline-exhausted';
            END IF;
            IF NEW.failure_kind = 'effect-uncertain'
               AND NOT EXISTS (
                   SELECT 1 FROM wamn_run.runs AS run
                   WHERE run.tenant_id = NEW.tenant_id
                     AND run.run_id = NEW.run_id
                     AND run.status = 'effect-uncertain'
               ) THEN
                RAISE EXCEPTION USING ERRCODE = '55000',
                    MESSAGE = 'authoring-test-case-not-effect-uncertain';
            END IF;
            IF (NEW.passed OR NEW.failure_kind <> 'deadline-exhausted')
               AND (NEW.resolution_map IS NULL
                    OR NEW.resolution_map = '{}'::jsonb) THEN
                RAISE EXCEPTION USING ERRCODE = '23514',
                    MESSAGE = 'authoring-test-case-resolution-map-required';
            END IF;
            IF NEW.resolution_map IS NOT NULL THEN
                IF NOT EXISTS (
                    SELECT 1 FROM wamn_run.runs AS run
                    WHERE run.tenant_id = NEW.tenant_id
                      AND run.run_id = NEW.run_id
                      AND run.catalog_id = NEW.catalog_id
                      AND run.catalog_version = NEW.catalog_version
                      AND run.invocation_context #>>
                          '{principal,validated-draft-hash}' = NEW.validated_draft_id
                      AND run.invocation_context #>>
                          '{source,report-id}' = NEW.report_id
                      AND run.invocation_context #>>
                          '{source,case-id}' = NEW.case_id
                ) THEN
                    RAISE EXCEPTION USING ERRCODE = '23514',
                        MESSAGE = 'authoring-test-case-run-pin-mismatch';
                END IF;
                SELECT COALESCE(
                    jsonb_object_agg(map.flow_id, map.execution_bundle_hash
                                     ORDER BY map.flow_id), '{}'::jsonb
                ) INTO actual_resolution_map
                FROM wamn_run.run_flow_resolutions AS map
                WHERE map.tenant_id = NEW.tenant_id
                  AND map.run_id = NEW.run_id;
                IF NEW.resolution_map <> actual_resolution_map THEN
                    RAISE EXCEPTION USING ERRCODE = '23514',
                        MESSAGE = 'authoring-test-case-resolution-map-invalid';
                END IF;
            END IF;
            IF NEW.passed AND reservation.resolution_map IS DISTINCT FROM NEW.resolution_map THEN
                RAISE EXCEPTION USING ERRCODE = '23514',
                    MESSAGE = 'authoring-test-case-resolution-map-mismatch';
            END IF;
            IF NEW.failure_kind = 'resolution-map-mismatch'
               AND reservation.resolution_map IS NOT DISTINCT FROM NEW.resolution_map THEN
                RAISE EXCEPTION USING ERRCODE = '23514',
                    MESSAGE = 'authoring-test-case-resolution-map-match';
            END IF;
        ELSE
            RAISE EXCEPTION USING ERRCODE = '55000',
                MESSAGE = 'authoring-test-case-unexpected-operation';
        END IF;
    ELSIF TG_TABLE_NAME = 'authoring_test_reports' AND TG_OP = 'INSERT' THEN
        SELECT * INTO reservation
        FROM wamn_run.authoring_test_run_reservations AS candidate
        WHERE candidate.tenant_id = NEW.tenant_id
          AND candidate.report_id = NEW.report_id
        FOR UPDATE;
        IF NOT FOUND OR reservation.state <> 'pending'
           OR NEW.validated_draft_id <> reservation.validated_draft_id
           OR NEW.test_set_hash <> reservation.test_set_hash
           OR NEW.catalog_id <> reservation.catalog_id
           OR NEW.catalog_version <> reservation.catalog_version
           OR NEW.resolution_map <> COALESCE(reservation.resolution_map, '{}'::jsonb) THEN
            RAISE EXCEPTION USING ERRCODE = '23514',
                MESSAGE = 'authoring-test-report-reservation-mismatch';
        END IF;
        SELECT count(*), count(*) FILTER (WHERE test_case.state = 'finalized'),
               COALESCE(bool_and(test_case.passed), false),
               COALESCE(bool_and(
                   test_case.resolution_map IS NULL
                   OR test_case.resolution_map = NEW.resolution_map
                   OR test_case.failure_kind = 'resolution-map-mismatch'
               ), true),
               jsonb_build_object('cases', jsonb_agg(
                   jsonb_build_object(
                       'ordinal', test_case.ordinal,
                       'case-id', test_case.case_id,
                       'run-id', test_case.run_id,
                       'passed', test_case.passed,
                       'failure-kind', test_case.failure_kind,
                       'summary', test_case.summary
                   ) ORDER BY test_case.ordinal
               ))
          INTO expected_count, finalized_count, all_passed, every_map_matches,
               expected_summary
          FROM wamn_run.authoring_test_case_runs AS test_case
         WHERE test_case.tenant_id = NEW.tenant_id
           AND test_case.report_id = NEW.report_id;
        IF expected_count <> reservation.case_count
           OR finalized_count <> reservation.case_count
           OR NOT every_map_matches
           OR NEW.passed IS DISTINCT FROM all_passed
           OR NEW.summary IS DISTINCT FROM expected_summary THEN
            RAISE EXCEPTION USING ERRCODE = '23514',
                MESSAGE = 'authoring-test-report-summary-mismatch';
        END IF;
    ELSE
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'authoring-test-orchestration-unexpected-write';
    END IF;
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION wamn_run.guard_authoring_test_orchestration_write()
    FROM PUBLIC;

CREATE TRIGGER authoring_test_run_reservations_controlled_insert
BEFORE INSERT ON wamn_run.authoring_test_run_reservations
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_authoring_test_orchestration_write();
CREATE TRIGGER authoring_test_run_reservations_controlled_update
BEFORE UPDATE ON wamn_run.authoring_test_run_reservations
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_authoring_test_orchestration_write();
CREATE TRIGGER authoring_test_run_reservations_delete_immutable
BEFORE DELETE ON wamn_run.authoring_test_run_reservations
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_authoring_test_orchestration_change();

CREATE TRIGGER authoring_test_case_runs_controlled_insert
BEFORE INSERT ON wamn_run.authoring_test_case_runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_authoring_test_orchestration_write();
CREATE TRIGGER authoring_test_case_runs_controlled_update
BEFORE UPDATE ON wamn_run.authoring_test_case_runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_authoring_test_orchestration_write();
CREATE TRIGGER authoring_test_case_runs_delete_immutable
BEFORE DELETE ON wamn_run.authoring_test_case_runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_authoring_test_orchestration_change();

CREATE TRIGGER authoring_test_reports_controlled_insert
BEFORE INSERT ON wamn_run.authoring_test_reports
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_authoring_test_orchestration_write();
CREATE TRIGGER authoring_test_reports_update_immutable
BEFORE UPDATE ON wamn_run.authoring_test_reports
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_authoring_test_orchestration_change();
CREATE TRIGGER authoring_test_reports_delete_immutable
BEFORE DELETE ON wamn_run.authoring_test_reports
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_authoring_test_orchestration_change();

-- BEGIN AUTHORING REPORT STORAGE MIGRATION (wamn-ftfc.11)
-- Report retention is run-plane-owned: this artifact remains independently
-- applicable before the catalog schema exists. The reconciler owns these
-- helpers explicitly, so missing report tables never depend on a control-plane
-- function. Only the separately credentialed host author may write.
GRANT USAGE ON SCHEMA wamn_run TO wamn_scenario_author;

CREATE OR REPLACE FUNCTION wamn_run.reject_immutable_authoring_report_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = 'authoring-report-immutable';
END
$$;
REVOKE ALL ON FUNCTION wamn_run.reject_immutable_authoring_report_change() FROM PUBLIC;

CREATE OR REPLACE FUNCTION wamn_run.guard_authoring_report_write()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    new_row jsonb := to_jsonb(NEW);
    old_row jsonb := CASE WHEN TG_OP = 'UPDATE' THEN to_jsonb(OLD) END;
    reservation_command jsonb;
    expected_case_count bigint;
    actual_case_count bigint;
    max_fact_ordinal int;
    all_facts_passed boolean;
BEGIN
    IF TG_TABLE_NAME = 'authoring_report_reservations' THEN
        IF TG_OP = 'INSERT' THEN
            IF new_row ->> 'state' <> 'pending'
               OR new_row -> 'finalized_at' <> 'null'::jsonb THEN
                RAISE EXCEPTION USING
                    ERRCODE = '55000',
                    MESSAGE = 'authoring-report-reservation-must-start-pending';
            END IF;
            IF jsonb_typeof(new_row -> 'command_json' -> 'cases')
                   IS DISTINCT FROM 'array' THEN
                RAISE EXCEPTION USING
                    ERRCODE = '23514',
                    MESSAGE = 'authoring-report-command-cases-invalid';
            END IF;
            IF jsonb_array_length(new_row -> 'command_json' -> 'cases')
                   > 2147483647 THEN
                RAISE EXCEPTION USING
                    ERRCODE = '23514',
                    MESSAGE = 'authoring-report-command-cases-invalid';
            END IF;
            IF EXISTS (
                SELECT 1
                FROM jsonb_array_elements(
                    new_row -> 'command_json' -> 'cases'
                ) AS command_case(value)
                WHERE jsonb_typeof(command_case.value) <> 'object'
                   OR NULLIF(command_case.value ->> 'case-id', '') IS NULL
                   OR NULLIF(command_case.value ->> 'case-content-hash', '') IS NULL
                   OR NULLIF(command_case.value ->> 'run-id', '') IS NULL
                   OR NULLIF(command_case.value ->> 'execution-schema', '') IS NULL
            ) OR EXISTS (
                SELECT 1
                FROM jsonb_array_elements(
                    new_row -> 'command_json' -> 'cases'
                ) AS command_case(value)
                GROUP BY command_case.value ->> 'case-id'
                HAVING count(*) > 1
            ) OR EXISTS (
                SELECT 1
                FROM jsonb_array_elements(
                    new_row -> 'command_json' -> 'cases'
                ) AS command_case(value)
                GROUP BY command_case.value ->> 'run-id'
                HAVING count(*) > 1
            ) THEN
                RAISE EXCEPTION USING
                    ERRCODE = '23514',
                    MESSAGE = 'authoring-report-command-cases-invalid';
            END IF;
        ELSIF TG_OP = 'UPDATE' THEN
            IF (new_row - 'state' - 'finalized_at')
                   IS DISTINCT FROM (old_row - 'state' - 'finalized_at')
               OR old_row ->> 'state' <> 'pending'
               OR new_row ->> 'state' <> 'finalized'
               OR new_row -> 'finalized_at' = 'null'::jsonb
               OR NOT EXISTS (
                   SELECT 1 FROM wamn_run.authoring_suite_reports AS report
                   WHERE report.tenant_id = old_row ->> 'tenant_id'
                     AND report.report_id = old_row ->> 'report_id'
               ) THEN
                RAISE EXCEPTION USING
                    ERRCODE = '55000',
                    MESSAGE = 'authoring-report-reservation-uncontrolled-update';
            END IF;
        ELSE
            RAISE EXCEPTION USING
                ERRCODE = '55000',
                MESSAGE = 'authoring-report-reservation-unexpected-operation';
        END IF;
    ELSIF TG_TABLE_NAME = 'authoring_suite_case_facts' AND TG_OP = 'INSERT' THEN
        SELECT reservation.command_json INTO reservation_command
        FROM wamn_run.authoring_report_reservations AS reservation
        WHERE reservation.tenant_id = new_row ->> 'tenant_id'
          AND reservation.report_id = new_row ->> 'report_id'
          AND reservation.state = 'pending'
        FOR UPDATE;
        IF reservation_command IS NULL OR NOT EXISTS (
            SELECT 1
            FROM jsonb_array_elements(
                reservation_command -> 'cases'
            ) WITH ORDINALITY AS command_case(value, position)
            WHERE command_case.position - 1
                    = (new_row ->> 'ordinal')::bigint
              AND command_case.value ->> 'case-id'
                    = new_row ->> 'case_id'
              AND command_case.value ->> 'run-id'
                    = new_row ->> 'run_id'
              AND NULLIF(command_case.value ->> 'case-content-hash', '')
                    IS NOT NULL
        ) THEN
            RAISE EXCEPTION USING
                ERRCODE = '23514',
                MESSAGE = 'authoring-case-fact-command-mismatch';
        END IF;
    ELSIF TG_TABLE_NAME = 'authoring_suite_reports' AND TG_OP = 'INSERT' THEN
        SELECT reservation.command_json INTO reservation_command
        FROM wamn_run.authoring_report_reservations AS reservation
        WHERE reservation.tenant_id = new_row ->> 'tenant_id'
          AND reservation.report_id = new_row ->> 'report_id'
          AND reservation.execution_id = new_row ->> 'execution_id'
          AND reservation.flow_id = new_row ->> 'flow_id'
          AND reservation.suite_flow_version
                = (new_row ->> 'suite_flow_version')::int
          AND reservation.suite_id = new_row ->> 'suite_id'
          AND reservation.lineage_json = new_row -> 'lineage_json'
          AND reservation.lineage_hash = new_row ->> 'lineage_hash'
          AND reservation.state = 'pending'
        FOR UPDATE;
        IF reservation_command IS NULL THEN
            RAISE EXCEPTION USING
                ERRCODE = '23514',
                MESSAGE = 'authoring-report-reservation-mismatch';
        END IF;

        expected_case_count := jsonb_array_length(
            reservation_command -> 'cases'
        );
        SELECT count(*), COALESCE(max(fact.ordinal), -1),
               COALESCE(bool_and(fact.passed), true)
        INTO actual_case_count, max_fact_ordinal, all_facts_passed
        FROM wamn_run.authoring_suite_case_facts AS fact
        WHERE fact.tenant_id = new_row ->> 'tenant_id'
          AND fact.report_id = new_row ->> 'report_id';

        IF (new_row -> 'refusal' = 'null'::jsonb
            AND actual_case_count <> expected_case_count)
           OR (new_row -> 'refusal' <> 'null'::jsonb
               AND actual_case_count > expected_case_count)
           OR max_fact_ordinal <> actual_case_count - 1 THEN
            RAISE EXCEPTION USING
                ERRCODE = '23514',
                MESSAGE = 'authoring-report-case-cardinality-mismatch';
        END IF;
        IF (new_row ->> 'passed')::boolean IS DISTINCT FROM
           (new_row -> 'refusal' = 'null'::jsonb AND all_facts_passed) THEN
            RAISE EXCEPTION USING
                ERRCODE = '23514',
                MESSAGE = 'authoring-report-summary-mismatch';
        END IF;
    ELSE
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'authoring-report-unexpected-write';
    END IF;
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION wamn_run.guard_authoring_report_write() FROM PUBLIC;

-- A reservation is durable before the first case admission. Its command binds
-- the ordered case identities/content hashes, target mode, and observation-
-- affecting options loaded in one snapshot. A retry may reuse exact facts but
-- may not silently change that command or lineage.
CREATE TABLE wamn_run.authoring_report_reservations (
    tenant_id          text NOT NULL CHECK (tenant_id <> ''),
    report_id          text NOT NULL CHECK (report_id <> ''),
    execution_id       text NOT NULL CHECK (execution_id <> ''),
    flow_id            text NOT NULL CHECK (flow_id <> ''),
    suite_flow_version int NOT NULL CHECK (suite_flow_version > 0),
    suite_id           text NOT NULL CHECK (suite_id <> ''),
    command_json       jsonb NOT NULL CHECK (jsonb_typeof(command_json) = 'object'),
    command_hash       text NOT NULL CHECK (command_hash <> ''),
    lineage_json       jsonb NOT NULL CHECK (
        jsonb_typeof(lineage_json) = 'object'
        AND lineage_json ->> 'kind' IN ('draft', 'release')
    ),
    lineage_hash       text NOT NULL CHECK (lineage_hash <> ''),
    state              text NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'finalized')),
    created_at         timestamptz NOT NULL DEFAULT now(),
    finalized_at       timestamptz,
    PRIMARY KEY (tenant_id, report_id),
    UNIQUE (tenant_id, execution_id),
    UNIQUE (
        tenant_id, report_id, execution_id, flow_id, suite_flow_version,
        suite_id, lineage_hash
    ),
    CONSTRAINT authoring_report_reservations_finalization_pair CHECK (
        (state = 'pending' AND finalized_at IS NULL)
        OR (state = 'finalized' AND finalized_at IS NOT NULL
            AND finalized_at >= created_at)
    )
);
ALTER TABLE wamn_run.authoring_report_reservations ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.authoring_report_reservations FORCE ROW LEVEL SECURITY;
CREATE POLICY authoring_report_reservations_tenant
    ON wamn_run.authoring_report_reservations
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE ON wamn_run.authoring_report_reservations
    TO wamn_scenario_author;

CREATE TRIGGER authoring_report_reservations_controlled_insert
BEFORE INSERT ON wamn_run.authoring_report_reservations
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_authoring_report_write();
CREATE TRIGGER authoring_report_reservations_controlled_update
BEFORE UPDATE ON wamn_run.authoring_report_reservations
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_authoring_report_write();
CREATE TRIGGER authoring_report_reservations_delete_immutable
BEFORE DELETE ON wamn_run.authoring_report_reservations
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_authoring_report_change();

-- Case facts are append-only beneath the reservation, not the final summary.
-- Thus a crash cannot erase already observed outcomes or force an admitted run
-- to execute again merely to reconstruct a report.
CREATE TABLE wamn_run.authoring_suite_case_facts (
    tenant_id   text NOT NULL CHECK (tenant_id <> ''),
    report_id   text NOT NULL CHECK (report_id <> ''),
    ordinal     int NOT NULL CHECK (ordinal >= 0),
    case_id     text NOT NULL CHECK (case_id <> ''),
    run_id      text NOT NULL CHECK (run_id <> ''),
    passed      boolean NOT NULL,
    status      text NOT NULL CHECK (
        status IN ('dispatched', 'running', 'completed', 'failed',
                   'infrastructure-failure', 'effect-uncertain')
    ),
    fail_kind   text CHECK (
        fail_kind IN ('terminal', 'retry-exhausted', 'invalid-input',
                      'runaway-budget', 'effect-uncertain')
    ),
    fail_node   text,
    outcome     jsonb NOT NULL CHECK (jsonb_typeof(outcome) = 'object'),
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, report_id, ordinal),
    UNIQUE (tenant_id, report_id, case_id),
    UNIQUE (tenant_id, run_id),
    FOREIGN KEY (tenant_id, report_id)
        REFERENCES wamn_run.authoring_report_reservations (tenant_id, report_id)
);
ALTER TABLE wamn_run.authoring_suite_case_facts ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.authoring_suite_case_facts FORCE ROW LEVEL SECURITY;
CREATE POLICY authoring_suite_case_facts_tenant
    ON wamn_run.authoring_suite_case_facts
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT ON wamn_run.authoring_suite_case_facts TO wamn_scenario_author;

CREATE TRIGGER authoring_suite_case_facts_require_pending
BEFORE INSERT ON wamn_run.authoring_suite_case_facts
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_authoring_report_write();
CREATE TRIGGER authoring_suite_case_facts_update_immutable
BEFORE UPDATE ON wamn_run.authoring_suite_case_facts
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_authoring_report_change();
CREATE TRIGGER authoring_suite_case_facts_delete_immutable
BEFORE DELETE ON wamn_run.authoring_suite_case_facts
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_authoring_report_change();

-- The final summary is immutable and exact-lineage-bound to its reservation.
-- Advancing a draft or changing a suite later cannot move the recorded origin.
CREATE TABLE wamn_run.authoring_suite_reports (
    tenant_id          text NOT NULL CHECK (tenant_id <> ''),
    report_id          text NOT NULL CHECK (report_id <> ''),
    execution_id       text NOT NULL CHECK (execution_id <> ''),
    flow_id            text NOT NULL CHECK (flow_id <> ''),
    suite_flow_version int NOT NULL CHECK (suite_flow_version > 0),
    suite_id           text NOT NULL CHECK (suite_id <> ''),
    passed             boolean NOT NULL,
    lineage_json       jsonb NOT NULL CHECK (
        jsonb_typeof(lineage_json) = 'object'
        AND lineage_json ->> 'kind' IN ('draft', 'release')
    ),
    lineage_hash       text NOT NULL CHECK (lineage_hash <> ''),
    edit_to_run_ms     bigint CHECK (edit_to_run_ms IS NULL OR edit_to_run_ms >= 0),
    refusal            jsonb CHECK (refusal IS NULL OR jsonb_typeof(refusal) = 'object'),
    created_at         timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, report_id),
    UNIQUE (tenant_id, execution_id),
    FOREIGN KEY (
        tenant_id, report_id, execution_id, flow_id, suite_flow_version,
        suite_id, lineage_hash
    ) REFERENCES wamn_run.authoring_report_reservations (
        tenant_id, report_id, execution_id, flow_id, suite_flow_version,
        suite_id, lineage_hash
    )
);
ALTER TABLE wamn_run.authoring_suite_reports ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.authoring_suite_reports FORCE ROW LEVEL SECURITY;
CREATE POLICY authoring_suite_reports_tenant ON wamn_run.authoring_suite_reports
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT ON wamn_run.authoring_suite_reports TO wamn_scenario_author;
CREATE INDEX authoring_suite_reports_flow
    ON wamn_run.authoring_suite_reports (tenant_id, flow_id, created_at);

CREATE TRIGGER authoring_suite_reports_require_reservation
BEFORE INSERT ON wamn_run.authoring_suite_reports
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_authoring_report_write();
CREATE TRIGGER authoring_suite_reports_update_immutable
BEFORE UPDATE ON wamn_run.authoring_suite_reports
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_authoring_report_change();
CREATE TRIGGER authoring_suite_reports_delete_immutable
BEFORE DELETE ON wamn_run.authoring_suite_reports
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_authoring_report_change();
-- END AUTHORING REPORT STORAGE MIGRATION (wamn-ftfc.11)
