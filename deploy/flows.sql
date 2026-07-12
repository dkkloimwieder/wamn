-- Flow registry storage (5.14 registry / POC-F1). One row per (flow, version)
-- of a project's flow graphs: `graph_json` is the canonical wamn-flow (5.1)
-- document, `active` marks the version the runtime serves. Consumers: the
-- trigger dispatcher's registry sweep (`active_flows_sql`, crates/wamn-run-queue
-- — cron + row-event flows), the webhook-entry ingress (sync webhook route
-- matching), and the flowrunner guest (`load_active_flow`). Registration is the
-- deploy tooling's job (`wamn-host publish-catalog --flow`, which also enforces
-- that the column `flow_id` equals the graph's embedded flow-id — run ids are
-- minted from the column, so the 5.1 slug rule extends to it by equality).
--
-- STANDALONE ARTIFACT, ADDITIVE to deploy/run-state.sql: same convention as
-- run-queue.sql / catalog-schema.sql — deliberately NOT included by
-- deploy/postgres-init.sql (the s3.* gate fixtures keep their own stand-in).
-- Assumes deploy/run-state.sql has been applied first (schema `wamn_run` and
-- the `wamn_app` role). Provisioning a per-project schema rewrites `wamn_run`
-- to the project schema (`wamn-host publish-catalog --runstate`).
--
-- Security shape mirrors the rest of the platform: FORCE RLS keyed on the
-- `app.tenant` claim the wamn:postgres plugin injects; NULL claim => zero rows.

CREATE TABLE wamn_run.flows (
    tenant_id  text NOT NULL,
    flow_id    text NOT NULL,
    version    int  NOT NULL,
    active     boolean NOT NULL DEFAULT false,
    graph_json jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, flow_id, version)
);
-- The registry scan: active versions only (dispatcher sweep + ingress routing).
CREATE INDEX flows_active ON wamn_run.flows (tenant_id, flow_id) WHERE active;
ALTER TABLE wamn_run.flows ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.flows FORCE ROW LEVEL SECURITY;
CREATE POLICY flows_tenant ON wamn_run.flows
    USING (tenant_id = current_setting('app.tenant', true))
    WITH CHECK (tenant_id = current_setting('app.tenant', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.flows TO wamn_app;
