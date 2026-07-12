-- Run-state storage schema (5.7). The production tables that PERSIST flow
-- execution: `runs` (one row per execution) and `node_runs` (one row per node
-- execution). This is the durable, queryable record behind run history, at-
-- least-once execution, branch-aware replay, and partial re-run — the durable
-- half of what the pure engine (crates/wamn-runner, 5.2) left as an in-memory
-- seam. The reconstruction/partial-re-run LOGIC lives in crates/wamn-run-store;
-- these tables are the shape it reads and the driver (components/flowrunner)
-- writes.
--
-- STANDALONE ARTIFACT: deliberately NOT included by deploy/postgres-init.sql, the
-- same convention as deploy/catalog-schema.sql (3.1/3.4/3.5/3.6). The S3/S6 gate
-- fixtures carry their own `runs`/`node_runs` copies (postgres-init.sql schema
-- `s3`, and the testhostbench ephemeral clone) so the flowbench/testhostbench
-- gates exercise the rewired runner; this file is the production schema and the
-- target of the crate's live-apply gate. Assumes a pre-existing `wamn_app` role
-- (LOGIN, NOSUPERUSER, NOBYPASSRLS), exactly as catalog-schema.sql does.
--
-- Security shape mirrors the rest of the platform (s2/s3, catalog): tenant
-- separation purely via the `app.tenant` claim the wamn:postgres plugin injects
-- with SET LOCAL. Every table FORCEs RLS keyed on
-- current_setting('app.tenant', true), which is NULL (=> zero rows) when no
-- claim was injected.
--
-- SCOPE (what 5.7 does NOT own, reserved as nullable seams below): the durable
-- run QUEUE + leases + doorbell (5.14) co-transact with these INSERTs but own
-- their own table; the node-level I/O CAPTURE policy (9.6 — scrub/truncate/toggle)
-- fills `input_json`/`output_json`/`preview_head`/`redacted`; the content-
-- addressed payload BYTE store (5.10) is pointed at by `input_ref`/`output_ref`
-- + the `preview_head`/`payload_size`/`payload_hash` preview.

CREATE SCHEMA IF NOT EXISTS wamn_run AUTHORIZATION CURRENT_USER;
GRANT USAGE ON SCHEMA wamn_run TO wamn_app;

-- ---------------------------------------------------------------------------
-- runs: one row per flow execution. `input_json` is the trigger payload replay
-- seeds the entry node with; `result_json` is the last node's output on
-- completion; `state_json` carries transient run state (e.g. a `delay` node's
-- parked-wake deadline). A replay/partial-re-run is a NEW row (fresh run_id)
-- linked to its origin via `replay_of` + `root_run_id`, so the original run's
-- history stays immutable (audit/billing-safe lineage). `idempotency_key` dedupes
-- at-least-once REDELIVERY of the same trigger (a partial-unique index); a replay
-- mints a fresh key. `fail_kind` mirrors the engine `FailKind` so history can
-- flag an upstream bug (`invalid-input`) apart from a terminal error or an
-- exhausted retry budget. Status values are exactly wamn_run_store::RunStatus
-- as_sql (tied to the crate by a drift-guard test).
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.runs (
    tenant_id       text NOT NULL,
    run_id          text NOT NULL,
    flow_id         text NOT NULL,
    flow_version    int  NOT NULL,
    status          text NOT NULL DEFAULT 'running'
        CHECK (status IN ('dispatched', 'running', 'completed', 'failed',
                          'cancelled', 'infrastructure-failure')),
    trigger_source  text,
    input_json      jsonb,
    result_json     jsonb,
    state_json      jsonb,
    idempotency_key text,
    replay_of       text,
    root_run_id     text,
    fail_kind       text CHECK (fail_kind IN ('terminal', 'retry-exhausted', 'invalid-input')),
    fail_node       text,
    fail_reason     text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, run_id)
);
-- At-least-once: a redelivered trigger with the same key collapses to one run.
CREATE UNIQUE INDEX runs_idempotency ON wamn_run.runs (tenant_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
-- History listing / lineage traversal.
CREATE INDEX runs_flow ON wamn_run.runs (tenant_id, flow_id, created_at);
CREATE INDEX runs_root ON wamn_run.runs (tenant_id, root_run_id) WHERE root_run_id IS NOT NULL;
-- Cron anchor recovery (5.14 dispatcher): a restarted dispatcher recovers each
-- cron flow's last-fired tick from max(run_id) over that flow's cron runs
-- (crates/wamn-run-queue cron_last_run_sql). This partial index serves that as
-- a backward index scan instead of a seq scan at production runs-table scale,
-- and stays small — only cron-triggered runs enter it.
CREATE INDEX runs_cron_anchor ON wamn_run.runs (tenant_id, flow_id, run_id)
    WHERE trigger_source = 'cron';
ALTER TABLE wamn_run.runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.runs FORCE ROW LEVEL SECURITY;
CREATE POLICY runs_tenant ON wamn_run.runs
    USING (tenant_id = current_setting('app.tenant', true))
    WITH CHECK (tenant_id = current_setting('app.tenant', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.runs TO wamn_app;

-- ---------------------------------------------------------------------------
-- node_runs: one row per node execution, the branch-aware reconstruction source.
-- The idempotency key is (tenant_id, run_id, node_id, occurrence): `occurrence`
-- disambiguates a node the flow LOOPS through (0 = first visit); retries of ONE
-- occurrence share the row and bump `attempt` — they never create new rows.
-- Reconstruction (crates/wamn-run-store) replays only COMPLETED rows
-- (status success/error) in `seq` order, folding each as an emission on
-- `output_port` carrying `output_json`; a `running`/`parked` row is an
-- outstanding node the driver re-dispatches. `input_json` is what a partial
-- re-run seeds the node with. The `*_ref` / `preview_*` / `capture_mode` /
-- `redacted` columns are RESERVED nullable seams for 5.10 (payload byte store)
-- and 9.6 (capture policy) — 5.7 leaves them null and stores I/O inline.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.node_runs (
    tenant_id     text NOT NULL,
    run_id        text NOT NULL,
    node_id       text NOT NULL,
    occurrence    int  NOT NULL DEFAULT 0,
    seq           int  NOT NULL,
    attempt       int  NOT NULL DEFAULT 0,
    status        text NOT NULL
        CHECK (status IN ('running', 'parked', 'success', 'error')),
    output_port   text,
    output_json   jsonb,
    input_json    jsonb,
    error_kind    text CHECK (error_kind IN ('retryable', 'rate-limited', 'terminal',
                                            'invalid-input', 'cancelled')),
    error_detail  jsonb,
    resume_at     timestamptz,
    -- Reserved seams (5.10 payload byte store / 9.6 capture policy):
    input_ref     text,
    output_ref    text,
    preview_head  text,
    payload_size  bigint,
    payload_hash  text,
    capture_mode  text,
    redacted      boolean NOT NULL DEFAULT false,
    started_at    timestamptz NOT NULL DEFAULT now(),
    ended_at      timestamptz,
    PRIMARY KEY (tenant_id, run_id, node_id, occurrence),
    FOREIGN KEY (tenant_id, run_id) REFERENCES wamn_run.runs (tenant_id, run_id) ON DELETE CASCADE
);
-- Reconstruction reads a run's completed rows in dispatch order.
CREATE INDEX node_runs_seq ON wamn_run.node_runs (tenant_id, run_id, seq);
ALTER TABLE wamn_run.node_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.node_runs FORCE ROW LEVEL SECURITY;
CREATE POLICY node_runs_tenant ON wamn_run.node_runs
    USING (tenant_id = current_setting('app.tenant', true))
    WITH CHECK (tenant_id = current_setting('app.tenant', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.node_runs TO wamn_app;
