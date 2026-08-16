-- Authoring test storage. Inline test-set definitions and their durable
-- orchestration facts are the only retained test persistence.
--
-- STANDALONE ARTIFACT, ADDITIVE to deploy/sql/run-state.sql: same convention as
-- flows.sql / run-queue.sql — deliberately NOT included by
-- deploy/sql/postgres-init.sql. Assumes deploy/sql/run-state.sql has been applied
-- first (the `wamn_run` schema, the guest-visible
-- `wamn_app` role, and the host-only `wamn_scenario_author` NOLOGIN role).
-- Provisioning a per-project schema rewrites `wamn_run` to the project
-- schema (`wamn-ctl publish-catalog --runstate`, reconcile-run-plane).
--
-- Security shape mirrors flows.sql: FORCE RLS keyed on
-- NULLIF(current_setting('app.tenant', true), ''); an empty/absent claim reads
-- as NULL => zero rows, and CHECK (tenant_id <> '') forbids a ''-tenant row.

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

-- Durable inline-test orchestration.
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
