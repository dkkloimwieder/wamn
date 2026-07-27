-- Run-state storage schema (5.7). The production tables that PERSIST flow
-- execution: `runs` (one row per execution) and `node_runs` (one row per node
-- execution). This is the durable, queryable record behind run history, at-
-- least-once execution, branch-aware replay, and partial re-run — the durable
-- half of what the pure engine (crates/execution/flow-engine, 5.2) left as an in-memory
-- seam. The reconstruction/partial-re-run LOGIC lives in crates/execution/run-state;
-- these tables are the shape it reads and the driver (components/execution/flowrunner)
-- writes.
--
-- STANDALONE ARTIFACT: deliberately NOT included by deploy/sql/postgres-init.sql, the
-- same convention as deploy/sql/catalog-schema.sql (3.1/3.4/3.5/3.6). The S3/S6 gate
-- fixtures carry their own `runs`/`node_runs` copies (postgres-init.sql schema
-- `s3`, and the testhostbench ephemeral clone) so the flowbench/testhostbench
-- gates exercise the rewired runner; this file is the production schema and the
-- target of the crate's live-apply gate. Assumes a pre-existing `wamn_app` role
-- (LOGIN, NOSUPERUSER, NOBYPASSRLS), exactly as catalog-schema.sql does.
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
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    run_id          text NOT NULL,
    flow_id         text NOT NULL,
    flow_version    int  NOT NULL,
    catalog_id      text,
    catalog_version bigint,
    attachment_id   text,
    registration_id text,
    status          text NOT NULL DEFAULT 'running'
        CHECK (status IN ('dispatched', 'running', 'completed', 'failed',
                          'cancelled', 'infrastructure-failure')),
    trigger_source  text,
    input_json      jsonb,
    result_json     jsonb,
    state_json      jsonb,
    invocation_context jsonb NOT NULL DEFAULT '{}'::jsonb,
    admission_context_version int NOT NULL DEFAULT 1
        CHECK (admission_context_version > 0),
    platform_revision text NOT NULL DEFAULT 'legacy',
    idempotency_key text,
    replay_of       text,
    root_run_id     text,
    parent_run_id   text,
    parent_node_id  text,
    parent_occurrence int,
    invoke_depth    int NOT NULL DEFAULT 0 CHECK (invoke_depth >= 0),
    waiting_child_run_id text,
    waiting_child_occurrence int,
    wait_generation bigint,
    caller_outcome_kind text
        CHECK (caller_outcome_kind IN ('responded', 'failed', 'cancelled')),
    caller_outcome_json jsonb,
    caller_http_status int CHECK (caller_http_status BETWEEN 100 AND 599),
    caller_release_node_id text,
    caller_outcome_hash text,
    caller_released_at timestamptz,
    response_deadline_at timestamptz,
    run_deadline_at timestamptz,
    cancel_requested_kind text,
    cancel_requested_at timestamptz,
    cancel_kind text,
    terminal_reason text,
    fail_kind       text CHECK (fail_kind IN ('terminal', 'retry-exhausted', 'invalid-input',
                                              'runaway-budget', 'effect-uncertain')),
    fail_node       text,
    fail_reason     text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CHECK ((catalog_id IS NULL) = (catalog_version IS NULL)),
    CHECK ((parent_run_id IS NULL) = (parent_node_id IS NULL)
       AND (parent_run_id IS NULL) = (parent_occurrence IS NULL)),
    CHECK ((waiting_child_run_id IS NULL) = (waiting_child_occurrence IS NULL)
       AND (waiting_child_run_id IS NULL) = (wait_generation IS NULL)),
    CHECK ((cancel_requested_kind IS NULL) = (cancel_requested_at IS NULL)),
    CHECK ((caller_released_at IS NULL) = (caller_outcome_kind IS NULL)),
    CHECK (caller_outcome_kind IS NULL OR caller_outcome_json IS NOT NULL),
    CHECK (caller_outcome_kind <> 'responded' OR caller_release_node_id IS NOT NULL),
    CHECK (response_deadline_at IS NULL OR run_deadline_at IS NULL
           OR response_deadline_at <= run_deadline_at),
    PRIMARY KEY (tenant_id, run_id)
);
-- At-least-once: a redelivered trigger with the same key collapses to one run.
CREATE UNIQUE INDEX runs_idempotency ON wamn_run.runs (tenant_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
-- History listing / lineage traversal.
CREATE INDEX runs_flow ON wamn_run.runs (tenant_id, flow_id, created_at);
CREATE INDEX runs_root ON wamn_run.runs (tenant_id, root_run_id) WHERE root_run_id IS NOT NULL;
CREATE UNIQUE INDEX runs_parent_occurrence ON wamn_run.runs
    (tenant_id, parent_run_id, parent_node_id, parent_occurrence)
    WHERE parent_run_id IS NOT NULL;
CREATE INDEX runs_waiting_child ON wamn_run.runs (tenant_id, waiting_child_run_id)
    WHERE waiting_child_run_id IS NOT NULL;
CREATE INDEX runs_response_deadline ON wamn_run.runs (tenant_id, response_deadline_at)
    WHERE caller_released_at IS NULL
      AND response_deadline_at IS NOT NULL
      AND status IN ('dispatched', 'running');
CREATE INDEX runs_run_deadline ON wamn_run.runs (tenant_id, run_deadline_at)
    WHERE run_deadline_at IS NOT NULL
      AND status IN ('dispatched', 'running');
CREATE INDEX runs_cancel_requested ON wamn_run.runs (tenant_id, cancel_requested_at)
    WHERE cancel_requested_at IS NOT NULL
      AND status IN ('dispatched', 'running');
-- Cron anchor recovery (5.14 dispatcher): a restarted dispatcher recovers each
-- cron flow's last-fired tick from max(run_id) over that flow's cron runs
-- (crates/execution/run-state/src/queue cron_last_run_sql). This partial index serves that as
-- a backward index scan instead of a seq scan at production runs-table scale,
-- and stays small — only cron-triggered runs enter it.
CREATE INDEX runs_cron_anchor ON wamn_run.runs (tenant_id, flow_id, run_id)
    WHERE trigger_source = 'cron';
ALTER TABLE wamn_run.runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.runs FORCE ROW LEVEL SECURITY;
CREATE POLICY runs_tenant ON wamn_run.runs
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.runs TO wamn_app;

-- HTTP invocation idempotency ledger (§6.2). The identity is intentionally
-- definition-independent: reusing a client key after a definition change must
-- find the old admission and return `idempotency-scope-changed`, never create a
-- second run. The named unique constraint is mapped to the transitions module's
-- typed `duplicate-identity` refusal.
CREATE TABLE wamn_run.invocation_admissions (
    tenant_id                  text NOT NULL CHECK (tenant_id <> ''),
    catalog_id                 text NOT NULL,
    environment                text NOT NULL,
    attachment_id              text NOT NULL,
    definition_hash            text NOT NULL,
    principal_digest           text NOT NULL,
    client_key_digest          text NOT NULL,
    client_request_fingerprint text NOT NULL,
    admitted_catalog_version   bigint NOT NULL,
    admitted_flow_version      int NOT NULL,
    run_id                     text NOT NULL,
    created_at                 timestamptz NOT NULL DEFAULT now(),
    expires_at                 timestamptz NOT NULL,
    CONSTRAINT invocation_admissions_identity UNIQUE
        (tenant_id, catalog_id, environment, attachment_id,
         principal_digest, client_key_digest),
    FOREIGN KEY (tenant_id, run_id)
        REFERENCES wamn_run.runs (tenant_id, run_id) ON DELETE CASCADE
);
CREATE INDEX invocation_admissions_run
    ON wamn_run.invocation_admissions (tenant_id, run_id);
CREATE INDEX invocation_admissions_expiry
    ON wamn_run.invocation_admissions (tenant_id, expires_at);
ALTER TABLE wamn_run.invocation_admissions ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.invocation_admissions FORCE ROW LEVEL SECURITY;
CREATE POLICY invocation_admissions_tenant ON wamn_run.invocation_admissions
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.invocation_admissions TO wamn_app;

-- ---------------------------------------------------------------------------
-- cron_anchor: the dispatcher's DURABLE last-fired cron tick per flow (5.14,
-- wamn-fqg.6). One row per cron flow; `last_tick` is the epoch-ms instant of
-- the most recent tick the dispatcher fired, upserted INSIDE the fire
-- transaction (co-transacted with the write-ahead run + enqueue). This
-- DECOUPLES cron dedupe from prunable run history: `cron_last_run_sql` recovers
-- the anchor from max(run_id) over the flow's cron RUNS, but 9.6 retention
-- (wamn-srb) prunes those runs when retention < the cron period — and a
-- vanished anchor makes an already-fired tick RE-FIRE (the write-ahead ON
-- CONFLICT cannot absorb it, because the conflicting run row was pruned). This
-- table is never pruned, so the anchor survives; the runs-based recovery
-- demotes to a BOOTSTRAP fallback for pre-anchor flows (those whose last fire
-- predates this table). The upsert is monotonic — GREATEST(existing, incoming)
-- — so a losing replica or a redelivered fire never rewinds the anchor.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.cron_anchor (
    tenant_id  text   NOT NULL CHECK (tenant_id <> ''),
    flow_id    text   NOT NULL,
    last_tick  bigint NOT NULL,
    PRIMARY KEY (tenant_id, flow_id)
);
ALTER TABLE wamn_run.cron_anchor ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.cron_anchor FORCE ROW LEVEL SECURITY;
CREATE POLICY cron_anchor_tenant ON wamn_run.cron_anchor
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.cron_anchor TO wamn_app;

-- ---------------------------------------------------------------------------
-- node_runs: one row per node execution, the branch-aware reconstruction source.
-- The idempotency key is (tenant_id, run_id, node_id, occurrence): `occurrence`
-- disambiguates a node the flow LOOPS through (0 = first visit); retries of ONE
-- occurrence share the row and bump `attempt` — they never create new rows.
-- Reconstruction (crates/execution/run-state) replays only COMPLETED rows
-- (status success/error) in `seq` order, folding each as an emission on
-- `output_port` carrying `output_json`; a `running`/`parked` row is an
-- outstanding node the driver re-dispatches. `input_json` is what a partial
-- re-run seeds the node with. The `*_ref` / `preview_*` / `capture_mode` /
-- `redacted` columns are RESERVED nullable seams for 5.10 (payload byte store)
-- and 9.6 (capture policy) — 5.7 leaves them null and stores I/O inline.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.node_runs (
    tenant_id     text NOT NULL CHECK (tenant_id <> ''),
    run_id        text NOT NULL,
    node_id       text NOT NULL,
    occurrence    int  NOT NULL DEFAULT 0,
    seq           int  NOT NULL,
    attempt       int  NOT NULL DEFAULT 0,
    status        text NOT NULL
        CHECK (status IN ('started', 'parked', 'success', 'error')),
    recovery_class text
        CHECK (recovery_class IN ('replay', 'idempotent-with-key', 'never-replay')),
    attempt_started_at timestamptz,
    attempt_deadline_at timestamptz,
    attempt_input_ref text,
    attempt_key text,
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
    CHECK ((status <> 'started') OR
           (recovery_class IS NOT NULL
            AND attempt_started_at IS NOT NULL
            AND attempt_deadline_at IS NOT NULL
            AND attempt_input_ref IS NOT NULL)),
    CHECK (attempt_deadline_at IS NULL OR attempt_started_at IS NULL
           OR attempt_started_at <= attempt_deadline_at),
    PRIMARY KEY (tenant_id, run_id, node_id, occurrence),
    FOREIGN KEY (tenant_id, run_id) REFERENCES wamn_run.runs (tenant_id, run_id) ON DELETE CASCADE
);
-- Reconstruction reads a run's completed rows in dispatch order.
CREATE INDEX node_runs_seq ON wamn_run.node_runs (tenant_id, run_id, seq);
ALTER TABLE wamn_run.node_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.node_runs FORCE ROW LEVEL SECURITY;
CREATE POLICY node_runs_tenant ON wamn_run.node_runs
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.node_runs TO wamn_app;
