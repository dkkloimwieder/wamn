-- Flow test-suite storage (11.2). A flow's test cases live as CATALOG DATA,
-- versioned WITH the flow they test: every suite/case row pins a concrete
-- `(flow_id, flow_version)`, and the FK to `wamn_run.flows(tenant_id, flow_id,
-- version)` ON DELETE CASCADE makes that binding structural — dropping a flow
-- version takes its suites (and their cases) with it, and a flow version and
-- its suite promote together through the copy-project-env definition path
-- (crates/wamn-ctl copy_project_env: flows in block 2, suites in block 5).
--
-- The case BODY is opaque jsonb in v0 (`case_body`): the canonical
-- case/assertion vocabulary is a sibling crate (wamn-testkit); at integration
-- the suite envelope (crates/wamn-flow-tests) gains a validate-on-write pass
-- against those serde types. `flow_version` is DENORMALIZED onto `test_cases`
-- (not reachable only through `test_suites`) — the event_registrations
-- precedent (deploy/sql/catalog-schema.sql): it is part of the composite FK to
-- the suite and lets the promote-copy scope cases by version without a join.
--
-- STANDALONE ARTIFACT, ADDITIVE to deploy/sql/flows.sql: same convention as
-- flows.sql / run-queue.sql — deliberately NOT included by
-- deploy/sql/postgres-init.sql. Assumes deploy/sql/flows.sql has been applied
-- first (the `flows` table this FKs, the `wamn_run` schema, the `wamn_app`
-- role). Provisioning a per-project schema rewrites `wamn_run` to the project
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
