-- Authoring test storage. A draft carries its own `cases`, so the durable
-- orchestration facts are the only retained test persistence.
--
-- STANDALONE ARTIFACT, ADDITIVE to deploy/sql/run-state.sql: same convention as
-- run-queue.sql — deliberately NOT included by
-- deploy/sql/postgres-init.sql. Assumes deploy/sql/run-state.sql has been applied
-- first (the `wamn_run` schema, the guest-visible
-- `wamn_app` role, and the host-only `wamn_scenario_author` NOLOGIN role).
-- Provisioning a per-project schema rewrites `wamn_run` to the project
-- schema (`wamn-ctl publish-catalog --runstate`, reconcile-run-plane).
--
-- Security shape mirrors run-state.sql: FORCE RLS keyed on
-- NULLIF(current_setting('app.tenant', true), ''); an empty/absent claim reads
-- as NULL => zero rows, and CHECK (tenant_id <> '') forbids a ''-tenant row.

-- Durable test-case orchestration. The cases a report ran are the ones the
-- validated draft carries, so `validated_draft_id` is their whole identity.
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
    catalog_id         text NOT NULL CHECK (catalog_id <> ''),
    catalog_version    int NOT NULL CHECK (catalog_version > 0),
    case_count         int NOT NULL CHECK (case_count BETWEEN 1 AND 256),
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
                         'effect-uncertain')
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

-- Final reports copy every publication-relevant pin.
CREATE TABLE wamn_run.authoring_test_reports (
    tenant_id          text NOT NULL CHECK (tenant_id <> ''),
    report_id          text NOT NULL CHECK (report_id <> ''),
    validated_draft_id text NOT NULL CHECK (validated_draft_id <> ''),
    catalog_id         text NOT NULL CHECK (catalog_id <> ''),
    catalog_version    int NOT NULL CHECK (catalog_version > 0),
    passed             boolean NOT NULL,
    summary            jsonb NOT NULL CHECK (jsonb_typeof(summary) = 'object'),
    finalized_at       timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, report_id),
    CONSTRAINT authoring_test_report_reservation_fk
        FOREIGN KEY (tenant_id, report_id)
        REFERENCES wamn_run.authoring_test_run_reservations (tenant_id, report_id)
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
    expected_summary jsonb;
BEGIN
    IF TG_TABLE_NAME = 'authoring_test_run_reservations' THEN
        IF TG_OP = 'INSERT' THEN
            IF NEW.state <> 'pending' OR NEW.finalized_at IS NOT NULL THEN
                RAISE EXCEPTION USING ERRCODE = '55000',
                    MESSAGE = 'authoring-test-reservation-must-start-pending';
            END IF;
        ELSIF TG_OP = 'UPDATE' THEN
            IF OLD.state = 'pending' AND NEW.state = 'finalized'
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
                        - 'summary' - 'finalized_at')
                   <> (old_row - 'state' - 'passed' - 'failure_kind'
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
            -- No cross-plane recheck of the 'effect-uncertain' outcome
            -- (wamn-0h0g.20.12): wamn-0h0g.8.18 moved these three relations to
            -- the CONTROL database while the project run plane stayed
            -- project-local, so the terminal status arrives at the store as an
            -- already-observed argument rather than a join. This arm was the
            -- last statement in the record still naming a project-local
            -- relation; a guard that cannot be satisfied from the plane it
            -- runs in is not enforcement.
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
           OR NEW.catalog_id <> reservation.catalog_id
           OR NEW.catalog_version <> reservation.catalog_version THEN
            RAISE EXCEPTION USING ERRCODE = '23514',
                MESSAGE = 'authoring-test-report-reservation-mismatch';
        END IF;
        SELECT count(*), count(*) FILTER (WHERE test_case.state = 'finalized'),
               COALESCE(bool_and(test_case.passed), false),
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
          INTO expected_count, finalized_count, all_passed, expected_summary
          FROM wamn_run.authoring_test_case_runs AS test_case
         WHERE test_case.tenant_id = NEW.tenant_id
           AND test_case.report_id = NEW.report_id;
        IF expected_count <> reservation.case_count
           OR finalized_count <> reservation.case_count
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
