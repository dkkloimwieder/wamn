-- Flow registry storage (5.14 registry / POC-F1). One row per (flow, version)
-- of a project's flow graphs: `graph_json` is the canonical wamn-flow (5.1)
-- document, `active` marks the version the runtime serves. Consumers: the
-- trigger dispatcher's registry sweep (`active_flows_sql`, crates/wamn-run-queue
-- — cron + row-event flows), the poc-webhook-f1 ingress (sync webhook route
-- matching), and the flowrunner guest (`load_active_flow`). Registration is the
-- deploy tooling's job (`wamn-ctl publish-catalog --flow`, which also enforces
-- that the column `flow_id` equals the graph's embedded flow-id — run ids are
-- minted from the column, so the 5.1 slug rule extends to it by equality — and
-- rejects a webhook path already served by another ACTIVE flow; the
-- flows_active_webhook_path unique index below is the race-proof backstop).
--
-- STANDALONE ARTIFACT, ADDITIVE to deploy/sql/run-state.sql: same convention as
-- run-queue.sql / catalog-schema.sql — deliberately NOT included by
-- deploy/sql/postgres-init.sql (the s3.* gate fixtures keep their own stand-in).
-- Assumes deploy/sql/run-state.sql has been applied first (schema `wamn_run` and
-- the `wamn_app` role). Provisioning a per-project schema rewrites `wamn_run`
-- to the project schema (`wamn-ctl publish-catalog --runstate`).
--
-- Security shape mirrors the rest of the platform: FORCE RLS keyed on
-- NULLIF(current_setting('app.tenant', true), ''); an empty/absent claim reads
-- as NULL => zero rows, and CHECK (tenant_id <> '') forbids a ''-tenant row.

CREATE TABLE wamn_run.flows (
    tenant_id  text NOT NULL CHECK (tenant_id <> ''),
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
-- At most ONE active flow per webhook path per tenant (any webhook trigger,
-- sync or async): the ingress routes a request path to a single flow, so a
-- second active claimant would be silently shadowed (the read is ORDER BY
-- flow_id — deterministic, but arbitrary). register_flow pre-checks and
-- rejects with a named error; this expression index is the guarantee under
-- concurrent registration. Pathless webhook triggers are unconstrained.
CREATE UNIQUE INDEX flows_active_webhook_path ON wamn_run.flows
    (tenant_id, (graph_json->'trigger'->>'path'))
    WHERE active
      AND graph_json->'trigger'->>'type' = 'webhook'
      AND graph_json->'trigger'->>'path' IS NOT NULL;
ALTER TABLE wamn_run.flows ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.flows FORCE ROW LEVEL SECURITY;
CREATE POLICY flows_tenant ON wamn_run.flows
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.flows TO wamn_app;
