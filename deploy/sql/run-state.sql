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
GRANT USAGE ON SCHEMA wamn_run TO wamn_scenario_author;

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

-- Disposition requests and per-attempt entries are append-only even for the
-- owning role; retention must remove them only through an explicit future
-- audit-retention protocol, never an ad-hoc UPDATE/DELETE.
CREATE FUNCTION wamn_run.reject_immutable_effect_fact_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = 'effect-disposition-immutable';
END
$$;
REVOKE ALL ON FUNCTION wamn_run.reject_immutable_effect_fact_change() FROM PUBLIC;

-- This rollout child exposes no runtime writer for immutable effect facts.
-- Only migration sessions that bypass forced RLS may append; the next runtime
-- child must replace this refusal with a distinct host-only adapter rather
-- than granting the guest-visible application role direct table authority.
CREATE FUNCTION wamn_run.guard_effect_fact_append()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    current_can_migrate boolean := COALESCE(
        (SELECT candidate.rolsuper OR candidate.rolbypassrls
         FROM pg_catalog.pg_roles AS candidate
         WHERE candidate.rolname = CURRENT_USER),
        false
    );
BEGIN
    IF NOT current_can_migrate THEN
        RAISE EXCEPTION USING
            ERRCODE = '42501',
            MESSAGE = 'effect-fact-append-requires-migration-authority';
    END IF;
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION wamn_run.guard_effect_fact_append() FROM PUBLIC;

-- Direct app-role inserts can never manufacture disposition audit. Any future
-- host or project adapter must cross this guard through separately authenticated,
-- explicitly privileged platform machinery; no runner-claim bridge is provided.
CREATE FUNCTION wamn_run.guard_effect_disposition_append()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    owner_name text := pg_catalog.pg_get_userbyid((
        SELECT rel.relowner
        FROM pg_catalog.pg_class AS rel
        WHERE rel.oid = TG_RELID
    ));
    current_is_super boolean := COALESCE(
        (SELECT candidate.rolsuper
         FROM pg_catalog.pg_roles AS candidate
         WHERE candidate.rolname = CURRENT_USER),
        false
    );
BEGIN
    IF NOT current_is_super
       AND NOT (CURRENT_USER = owner_name AND CURRENT_USER <> SESSION_USER) THEN
        RAISE EXCEPTION USING
            ERRCODE = '42501',
            MESSAGE = 'effect-disposition-append-requires-trusted-adapter';
    END IF;
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION wamn_run.guard_effect_disposition_append() FROM PUBLIC;

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
    environment     text,
    attachment_id   text,
    registration_id text,
    -- Trusted CDC causation is distinct from replay lineage. The immediate
    -- source lets admission verify the carried root/depth under tenant RLS.
    event_source_run_id text,
    event_root_run_id text,
    event_depth      int CHECK (event_depth BETWEEN 0 AND 16),
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
    invoke_root_run_id text,
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
    CHECK (environment IS NULL OR environment <> ''),
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
    CHECK ((parent_run_id IS NULL) = (parent_node_id IS NULL)
       AND (parent_run_id IS NULL) = (parent_occurrence IS NULL)),
    CHECK ((parent_run_id IS NULL) = (invoke_root_run_id IS NULL)),
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
CREATE INDEX runs_event_root ON wamn_run.runs (tenant_id, event_root_run_id)
    WHERE event_root_run_id IS NOT NULL;
CREATE UNIQUE INDEX runs_parent_occurrence ON wamn_run.runs
    (tenant_id, parent_run_id, parent_node_id, parent_occurrence)
    WHERE parent_run_id IS NOT NULL;
CREATE INDEX runs_invoke_root ON wamn_run.runs (tenant_id, invoke_root_run_id)
    WHERE invoke_root_run_id IS NOT NULL;
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

CREATE TRIGGER runs_event_lineage_immutable
BEFORE UPDATE OF event_source_run_id, event_root_run_id, event_depth
ON wamn_run.runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_event_lineage_immutable();
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.runs TO wamn_app;
GRANT SELECT ON wamn_run.runs TO wamn_scenario_author;

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
    CONSTRAINT invocation_admissions_run_fk FOREIGN KEY (tenant_id, run_id)
        REFERENCES wamn_run.runs (tenant_id, run_id) ON DELETE CASCADE
        DEFERRABLE INITIALLY DEFERRED
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
-- disambiguates a node the flow LOOPS through (0 = first visit). Effect
-- redispatches append a new immutable effect_attempts row and advance only the
-- constrained current pointer — they never copy authority facts onto this row.
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
    -- Mutable occurrence projection only: points at the current row in the
    -- append-only effect_attempts ledger. Never expose this pointer as an
    -- immutable attempt identity without joining that ledger.
    current_effect_attempt_id uuid,
    run_id        text NOT NULL,
    node_id       text NOT NULL,
    occurrence    int  NOT NULL DEFAULT 0,
    seq           int  NOT NULL,
    -- Compatibility projection retained through the immutable-ledger
    -- activation. New runtime authority moves to effect_attempts in the next
    -- ordered rollout child; legacy binaries must remain valid after this
    -- additive schema/reconcile child is published.
    attempt       int  NOT NULL DEFAULT 0,
    status        text NOT NULL
        CHECK (status IN ('started', 'parked', 'success', 'error')),
    selected_recovery_class text
        CHECK (selected_recovery_class IN ('replay', 'idempotent-with-key', 'never-replay')),
    recovery_class text
        CHECK (recovery_class IN ('replay', 'idempotent-with-key', 'never-replay')),
    generation_fact_kind text
        CHECK (generation_fact_kind IN ('not-required', 'attested')),
    connection_generation text,
    credential_generation text,
    attempt_started_at timestamptz,
    attempt_dispatched_at timestamptz,
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
           (selected_recovery_class IS NOT NULL
            AND recovery_class IS NOT NULL
            AND selected_recovery_class = recovery_class
            AND generation_fact_kind IS NOT NULL
            AND attempt_started_at IS NOT NULL
            AND attempt_deadline_at IS NOT NULL
            AND attempt_input_ref IS NOT NULL)),
    CHECK ((generation_fact_kind = 'not-required'
            AND connection_generation IS NULL
            AND credential_generation IS NULL)
           OR (generation_fact_kind = 'attested'
               AND connection_generation IS NOT NULL
               AND connection_generation <> ''
               AND credential_generation IS NOT NULL
               AND credential_generation <> '')),
    CHECK (attempt_deadline_at IS NULL OR attempt_started_at IS NULL
           OR attempt_started_at <= attempt_deadline_at),
    CHECK (attempt_dispatched_at IS NULL OR attempt_started_at IS NULL
           OR attempt_started_at <= attempt_dispatched_at),
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

-- ---------------------------------------------------------------------------
-- Immutable effect-attempt ledger. `node_runs` remains the current occurrence
-- projection; every actual dispatch generation gets its own server-minted id
-- here. A crash before the send boundary reuses its prepared row. An authorized
-- replay/idempotent redispatch appends a successor and advances only the
-- node_runs pointer. Never-replay cannot mint a successor.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.effect_attempts (
    tenant_id       text NOT NULL,
    attempt_id      uuid NOT NULL DEFAULT gen_random_uuid(),
    run_id          text NOT NULL,
    node_id         text NOT NULL,
    occurrence      int NOT NULL,
    seq             int NOT NULL,
    attempt_index   int NOT NULL,
    predecessor_attempt_id uuid,
    -- Legacy mutable rows had no predecessor identity. The migration marks
    -- that one exception explicitly; new successor attempts must carry typed
    -- same-occurrence lineage.
    legacy_imported boolean NOT NULL DEFAULT false,
    selected_recovery_class text NOT NULL,
    recovery_class  text NOT NULL,
    generation_fact_kind text NOT NULL,
    connection_name text,
    connection_generation text,
    credential_generation text,
    verified_author_principal text,
    verified_publisher_principal text,
    attempt_started_at timestamptz NOT NULL,
    attempt_deadline_at timestamptz NOT NULL,
    attempt_input_ref text NOT NULL,
    attempt_key text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT effect_attempts_tenant_check CHECK (tenant_id <> ''),
    CONSTRAINT effect_attempts_occurrence_check CHECK (occurrence >= 0),
    CONSTRAINT effect_attempts_seq_check CHECK (seq >= 0),
    CONSTRAINT effect_attempts_attempt_index_check CHECK (attempt_index >= 0),
    CONSTRAINT effect_attempts_lineage_check CHECK (
        (legacy_imported AND predecessor_attempt_id IS NULL)
        OR
        (NOT legacy_imported
         AND ((attempt_index = 0 AND predecessor_attempt_id IS NULL)
              OR (attempt_index > 0 AND predecessor_attempt_id IS NOT NULL)))
    ),
    CONSTRAINT effect_attempts_recovery_class_check
        CHECK (selected_recovery_class IN ('replay', 'idempotent-with-key', 'never-replay')
               AND recovery_class IN ('replay', 'idempotent-with-key', 'never-replay')
               AND selected_recovery_class = recovery_class),
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
    CONSTRAINT effect_attempts_key_check
        CHECK (recovery_class <> 'idempotent-with-key'
               OR (attempt_key IS NOT NULL AND attempt_key <> '')),
    PRIMARY KEY (tenant_id, attempt_id),
    UNIQUE (tenant_id, attempt_id, run_id, node_id, occurrence),
    UNIQUE (tenant_id, attempt_id, attempt_started_at),
    UNIQUE (tenant_id, run_id, node_id, occurrence, attempt_index),
    CONSTRAINT effect_attempts_predecessor_fk
        FOREIGN KEY (tenant_id, predecessor_attempt_id, run_id, node_id, occurrence)
        REFERENCES wamn_run.effect_attempts
            (tenant_id, attempt_id, run_id, node_id, occurrence)
);
-- Deliberately no FK from (tenant_id, run_id) to runs: effect attempts are an
-- audit ledger with an independent retention lifetime. Pruning terminal run
-- history cascades through the mutable node projection but must leave these
-- immutable facts intact. The current projection pointer below is constrained
-- in the opposite direction so it cannot name a nonexistent/cross-tenant fact.
CREATE INDEX effect_attempts_occurrence
    ON wamn_run.effect_attempts
       (tenant_id, run_id, node_id, occurrence, attempt_index);
CREATE INDEX effect_attempts_bulk_scope
    ON wamn_run.effect_attempts
       (tenant_id, connection_name, connection_generation, attempt_started_at);
ALTER TABLE wamn_run.effect_attempts ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.effect_attempts FORCE ROW LEVEL SECURITY;
CREATE POLICY effect_attempts_tenant ON wamn_run.effect_attempts
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON wamn_run.effect_attempts TO wamn_app;
REVOKE INSERT ON wamn_run.effect_attempts FROM wamn_app;
CREATE TRIGGER effect_attempts_insert_guard
BEFORE INSERT ON wamn_run.effect_attempts
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_effect_fact_append();
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
    dispatched_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT effect_attempt_dispatches_tenant_check CHECK (tenant_id <> ''),
    CONSTRAINT effect_attempt_dispatches_time_check
        CHECK (attempt_started_at <= dispatched_at),
    PRIMARY KEY (tenant_id, attempt_id),
    UNIQUE (tenant_id, attempt_id, dispatched_at),
    CONSTRAINT effect_attempt_dispatches_attempt_fk
        FOREIGN KEY (tenant_id, attempt_id, attempt_started_at)
        REFERENCES wamn_run.effect_attempts
            (tenant_id, attempt_id, attempt_started_at)
);
ALTER TABLE wamn_run.effect_attempt_dispatches ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.effect_attempt_dispatches FORCE ROW LEVEL SECURITY;
CREATE POLICY effect_attempt_dispatches_tenant ON wamn_run.effect_attempt_dispatches
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON wamn_run.effect_attempt_dispatches TO wamn_app;
REVOKE INSERT ON wamn_run.effect_attempt_dispatches FROM wamn_app;
CREATE TRIGGER effect_attempt_dispatches_insert_guard
BEFORE INSERT ON wamn_run.effect_attempt_dispatches
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_effect_fact_append();
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
GRANT SELECT ON wamn_run.effect_attempt_outcomes TO wamn_app;
REVOKE INSERT ON wamn_run.effect_attempt_outcomes FROM wamn_app;
CREATE TRIGGER effect_attempt_outcomes_insert_guard
BEFORE INSERT ON wamn_run.effect_attempt_outcomes
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_effect_fact_append();
CREATE TRIGGER effect_attempt_outcomes_update_immutable
BEFORE UPDATE ON wamn_run.effect_attempt_outcomes
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_effect_fact_change();
CREATE TRIGGER effect_attempt_outcomes_delete_immutable
BEFORE DELETE ON wamn_run.effect_attempt_outcomes
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_effect_fact_change();

-- ---------------------------------------------------------------------------
-- Effect disposition: immutable request envelope + exact materialized attempt
-- set. A resolution wakes the run; the runner consumes the complete asserted
-- outcome through the normal atomic completion/checkpoint transition, leaving
-- this audit ledger immutable. `selection_kind = bulk` records the required
-- stable query bounds; the per-attempt rows are the set actually authorized.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.effect_disposition_requests (
    tenant_id       text NOT NULL,
    request_id      uuid NOT NULL DEFAULT gen_random_uuid(),
    action          text NOT NULL,
    selection_kind  text NOT NULL,
    principal       text NOT NULL,
    effective_role  text NOT NULL,
    basis           text,
    evidence_ref    text,
    correlation_id  text NOT NULL,
    break_glass_reason text,
    connection_name text,
    connection_generation text,
    flow_id         text,
    window_start    timestamptz,
    window_end      timestamptz,
    -- Wall-clock audit time only. Per-attempt append order is carried by the
    -- immutable effect_dispositions.append_ordinal identity.
    created_at      timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT effect_disposition_requests_tenant_check CHECK (tenant_id <> ''),
    CONSTRAINT effect_disposition_requests_action_check
        CHECK (action IN ('park', 'release', 'resolve')),
    CONSTRAINT effect_disposition_requests_selection_check
        CHECK (selection_kind IN ('single', 'bulk')),
    CONSTRAINT effect_disposition_requests_principal_check CHECK (principal <> ''),
    CONSTRAINT effect_disposition_requests_role_check
        CHECK (effective_role IN ('system', 'project-deployer', 'project-admin',
                                  'platform-admin-break-glass')),
    CONSTRAINT effect_disposition_requests_role_action_check CHECK (
        (effective_role = 'system'
         AND action = 'park' AND selection_kind = 'single')
        OR
        (effective_role = 'project-deployer'
         AND action IN ('park', 'release'))
        OR effective_role IN ('project-admin', 'platform-admin-break-glass')
    ),
    CONSTRAINT effect_disposition_requests_basis_check
        CHECK (basis IS NULL OR basis IN ('external-evidence',
                                          'counterparty-confirmation',
                                          'operator-judgment')),
    CONSTRAINT effect_disposition_requests_correlation_check CHECK (correlation_id <> ''),
    CONSTRAINT effect_disposition_requests_resolution_audit_check CHECK (
        (action = 'resolve' AND basis IS NOT NULL
                            AND evidence_ref IS NOT NULL AND evidence_ref <> '')
        OR (action <> 'resolve' AND basis IS NULL)
    ),
    CONSTRAINT effect_disposition_requests_break_glass_check CHECK (
        (effective_role = 'platform-admin-break-glass'
         AND break_glass_reason IS NOT NULL AND break_glass_reason <> '')
        OR (effective_role <> 'platform-admin-break-glass'
            AND break_glass_reason IS NULL)
    ),
    CONSTRAINT effect_disposition_requests_bulk_bounds_check CHECK (
        selection_kind <> 'bulk'
        OR (connection_name IS NOT NULL AND connection_name <> ''
            AND connection_generation IS NOT NULL AND connection_generation <> ''
            AND window_start IS NOT NULL AND window_end IS NOT NULL
            AND isfinite(window_start) AND isfinite(window_end)
            AND window_start < window_end)
    ),
    CONSTRAINT effect_disposition_requests_single_filters_check CHECK (
        selection_kind <> 'single'
        OR (connection_name IS NULL AND connection_generation IS NULL
            AND flow_id IS NULL AND window_start IS NULL AND window_end IS NULL)
    ),
    PRIMARY KEY (tenant_id, request_id),
    UNIQUE (tenant_id, request_id, action)
);
ALTER TABLE wamn_run.effect_disposition_requests ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.effect_disposition_requests FORCE ROW LEVEL SECURITY;
CREATE POLICY effect_disposition_requests_tenant
    ON wamn_run.effect_disposition_requests
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON wamn_run.effect_disposition_requests TO wamn_app;
REVOKE INSERT ON wamn_run.effect_disposition_requests FROM wamn_app;
CREATE TRIGGER effect_disposition_requests_insert_guard
BEFORE INSERT ON wamn_run.effect_disposition_requests
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_effect_disposition_append();
CREATE TRIGGER effect_disposition_requests_update_immutable
BEFORE UPDATE ON wamn_run.effect_disposition_requests
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_effect_fact_change();
CREATE TRIGGER effect_disposition_requests_delete_immutable
BEFORE DELETE ON wamn_run.effect_disposition_requests
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_effect_fact_change();

CREATE TABLE wamn_run.effect_dispositions (
    tenant_id       text NOT NULL,
    request_id      uuid NOT NULL,
    attempt_id      uuid NOT NULL,
    -- Global immutable append order. This is the sole latest-history key;
    -- created_at below remains an audit timestamp and is never ordering truth.
    append_ordinal  bigint GENERATED ALWAYS AS IDENTITY,
    -- Stable position in the request's materialized exact attempt set. Single
    -- and automatic requests use zero; bulk orders by immutable attempt facts.
    selection_ordinal int NOT NULL DEFAULT 0,
    action          text NOT NULL,
    resolution_status text,
    success_payload jsonb,
    success_port    text,
    success_context jsonb,
    failure_kind    text,
    failure_detail  jsonb,
    created_at      timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT effect_dispositions_tenant_check CHECK (tenant_id <> ''),
    CONSTRAINT effect_dispositions_selection_ordinal_check
        CHECK (selection_ordinal >= 0),
    CONSTRAINT effect_dispositions_action_check
        CHECK (action IN ('park', 'release', 'resolve')),
    CONSTRAINT effect_dispositions_resolution_status_check
        CHECK (resolution_status IS NULL OR resolution_status IN ('succeeded', 'failed')),
    CONSTRAINT effect_dispositions_failure_kind_check
        CHECK (failure_kind IS NULL OR failure_kind IN ('terminal', 'invalid-input')),
    CONSTRAINT effect_dispositions_outcome_check CHECK ((
        (action <> 'resolve' AND resolution_status IS NULL
         AND success_payload IS NULL AND success_port IS NULL
         AND success_context IS NULL AND failure_kind IS NULL
         AND failure_detail IS NULL)
        OR
        (action = 'resolve' AND resolution_status = 'succeeded'
         AND success_payload IS NOT NULL
         AND success_port IS NOT NULL AND success_port <> ''
         AND (success_context IS NULL OR jsonb_typeof(success_context) = 'object')
         AND failure_kind IS NULL AND failure_detail IS NULL)
        OR
        (action = 'resolve' AND resolution_status = 'failed'
         AND success_payload IS NULL AND success_port IS NULL
         AND success_context IS NULL
         AND failure_kind IN ('terminal', 'invalid-input')
         AND failure_detail IS NOT NULL
         AND jsonb_typeof(failure_detail) = 'object'
         AND failure_detail ? 'message'
         AND jsonb_typeof(failure_detail -> 'message') = 'string'
         AND (NOT (failure_detail ? 'code')
              OR failure_detail -> 'code' = 'null'::jsonb
              OR jsonb_typeof(failure_detail -> 'code') = 'string'))
    ) IS TRUE),
    PRIMARY KEY (tenant_id, request_id, attempt_id),
    FOREIGN KEY (tenant_id, request_id, action)
        REFERENCES wamn_run.effect_disposition_requests (tenant_id, request_id, action),
    FOREIGN KEY (tenant_id, attempt_id)
        REFERENCES wamn_run.effect_attempts (tenant_id, attempt_id)
);
CREATE UNIQUE INDEX effect_dispositions_one_resolution
    ON wamn_run.effect_dispositions (tenant_id, attempt_id)
    WHERE action = 'resolve';
CREATE UNIQUE INDEX effect_dispositions_request_ordinal
    ON wamn_run.effect_dispositions (tenant_id, request_id, selection_ordinal);
CREATE UNIQUE INDEX effect_dispositions_append_order
    ON wamn_run.effect_dispositions (append_ordinal);
CREATE INDEX effect_dispositions_attempt_history
    ON wamn_run.effect_dispositions (tenant_id, attempt_id, append_ordinal DESC);
ALTER TABLE wamn_run.effect_dispositions ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.effect_dispositions FORCE ROW LEVEL SECURITY;
CREATE POLICY effect_dispositions_tenant ON wamn_run.effect_dispositions
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON wamn_run.effect_dispositions TO wamn_app;
REVOKE INSERT ON wamn_run.effect_dispositions FROM wamn_app;
CREATE TRIGGER effect_dispositions_insert_guard
BEFORE INSERT ON wamn_run.effect_dispositions
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_effect_disposition_append();
CREATE TRIGGER effect_dispositions_update_immutable
BEFORE UPDATE ON wamn_run.effect_dispositions
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_effect_fact_change();
CREATE TRIGGER effect_dispositions_delete_immutable
BEFORE DELETE ON wamn_run.effect_dispositions
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_effect_fact_change();

-- BEGIN POST-TABLE CONSTRAINTS
-- The full occurrence identity makes a cross-run/node pointer structurally
-- impossible. This lives after both tables so from-zero apply and additive
-- reconciliation can establish it without weakening the append-only ledger.
ALTER TABLE wamn_run.node_runs
    ADD CONSTRAINT node_runs_current_effect_attempt_fk
    FOREIGN KEY (tenant_id, current_effect_attempt_id, run_id, node_id, occurrence)
    REFERENCES wamn_run.effect_attempts
        (tenant_id, attempt_id, run_id, node_id, occurrence);
-- END POST-TABLE CONSTRAINTS
