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
