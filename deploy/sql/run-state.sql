-- Run-state storage schema (5.7). The production `runs` table persists one row
-- per execution. This is the durable, queryable record behind run history and
-- at-least-once execution. The router is a single-shot in-memory walk; these
-- tables are the facts its host-owned production driver writes.
--
-- Project provisioning installs the shared roles before applying this schema.
--
-- The database isolates the project. FORCE RLS requires a protected caller identity.
--
-- TARGET (what 5.7 does NOT own, reserved as nullable seams below): the durable
-- run QUEUE + leases (5.14) co-transact with these INSERTs but own
-- their own table; the run-level I/O CAPTURE policy (9.6) is fixed at admission
-- and full capture fills scrubbed `input_json`/`output_json` plus the output
-- size and optional hash; the content-addressed payload BYTE store (5.10) is
-- pointed at by `input_ref`/`output_ref`.

CREATE SCHEMA IF NOT EXISTS wamn_run AUTHORIZATION CURRENT_USER;

REVOKE ALL PRIVILEGES ON SCHEMA wamn_run
    FROM PUBLIC, wamn_platform;
GRANT USAGE ON SCHEMA wamn_run TO wamn_app;
GRANT USAGE ON SCHEMA wamn_run TO wamn_platform;

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
REVOKE ALL ON FUNCTION wamn_run.guard_event_lineage_immutable() FROM PUBLIC;

-- The workflow version and execution identity are fixed at admission.
CREATE FUNCTION wamn_run.guard_run_admission_pins_immutable()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.package_id IS DISTINCT FROM OLD.package_id
       OR NEW.environment IS DISTINCT FROM OLD.environment
       OR NEW.wiring_id IS DISTINCT FROM OLD.wiring_id
       OR NEW.wiring_version IS DISTINCT FROM OLD.wiring_version
       OR NEW.wiring_hash IS DISTINCT FROM OLD.wiring_hash
       OR NEW.trigger_source IS DISTINCT FROM OLD.trigger_source
       OR NEW.execution_principal_id IS DISTINCT FROM OLD.execution_principal_id
       OR NEW.execution_role IS DISTINCT FROM OLD.execution_role
       OR NEW.invoked_by IS DISTINCT FROM OLD.invoked_by
       OR NEW.invoked_by_user_id IS DISTINCT FROM OLD.invoked_by_user_id THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'run-admission-pin-immutable';
    END IF;
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION wamn_run.guard_run_admission_pins_immutable() FROM PUBLIC;

-- `wamn_platform` holds DELETE for history pruning, and `wamn_app` still
-- retains it pending the guest-login retirement. The table grant cannot express
-- the prune statement's terminal-state predicate, so this ordinary
-- invoker-rights trigger makes that predicate caller-independent.
--
-- THE TRIGGER AND THE ROLE ARE INDEPENDENT LAYERS AND STAY THAT WAY
-- (wamn-0h0g.12.69): the grant bounds WHO may delete, this trigger bounds WHAT
-- may be deleted, and neither is absorbed into the other. The predicates and
-- the failure modes differ, so each keeps its own probe.
CREATE FUNCTION wamn_run.guard_terminal_run_delete()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF OLD.status NOT IN ('completed', 'failed', 'infrastructure-failure') THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'run-delete-nonterminal';
    END IF;
    RETURN OLD;
END
$$;
REVOKE ALL ON FUNCTION wamn_run.guard_terminal_run_delete() FROM PUBLIC;

-- Owner ruling wamn-0h0g.20.7 (2026-08-21): the system env policy is projected
-- by `reconcile-run-plane`; publication verifies the projected source identity.
-- ---------------------------------------------------------------------------
-- runs: one row per workflow execution. `input_json` is the admitted trigger
-- payload; `result_json` is the last node's output on
-- completion. `state_json` stores the checkpoint at the Response.
-- The workflow identity and `idempotency_key` deduplicate redelivery. `fail_type` mirrors
-- the engine `FailType` so history can
-- flag an upstream bug (`invalid-input`) apart from a terminal error or an
-- exhausted retry budget. Status values are exactly wamn_run_store::RunStatus
-- as_sql (tied to the crate by a drift-guard test).
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.runs (
    run_id          text NOT NULL DEFAULT gen_random_uuid()::text,
    package_id      text NOT NULL,
    environment     text NOT NULL,
    -- Trusted immutable CDC event ancestry is distinct from retired execution
    -- lineage. The immediate source lets admission verify the carried root/depth
    -- under project RLS.
    event_source_run_id text,
    event_root_run_id text,
    event_depth      int CHECK (event_depth BETWEEN 0 AND 16),
    status          text NOT NULL DEFAULT 'running'
        CHECK (status IN ('dispatched', 'running', 'completed', 'failed',
                          'infrastructure-failure')),
    trigger_source  text,
    -- The principal and active role used when dispatching nodes.
    execution_principal_id uuid,
    execution_role  text,
    invoked_by      text,
    invoked_by_user_id uuid,
    -- The admission transaction freezes the execution identity.
    wiring_id       text,
    wiring_version  int,
    wiring_hash     text,
    input_json      jsonb,
    result_json     jsonb,
    deadline_adjustments_json jsonb,
    state_json      jsonb,
    admission_context_version text NOT NULL DEFAULT '0.1'
        CHECK (admission_context_version = '0.1'),
    idempotency_key text,
    terminal_reason text,
    fail_type       text CHECK (fail_type IN ('terminal', 'retry-exhausted', 'invalid-input',
                                              'runaway-budget',
                                              'unresolvable-name',
                                              'hash-invalid-bytes', 'foreign-revision',
                                              'incompatible-contract')),
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (package_id <> ''
           AND environment <> ''),
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
    -- The executor stamps `updated_at` from its own host clock (D2b), while
    -- `created_at` keeps the server default above. A worker whose clock runs
    -- behind the database would otherwise store a terminal state that looks
    -- older than the admission that created the run. Refuse that write.
    -- At insert both columns take the same `now()`, so equal is admitted.
    CONSTRAINT runs_updated_after_created
        CHECK (updated_at >= created_at),
    CONSTRAINT runs_wiring_identity_check CHECK (
      (wiring_id IS NULL AND wiring_version IS NULL)
      OR (wiring_id IS NOT NULL AND wiring_version IS NOT NULL
          AND wiring_id <> '' AND wiring_version > 0)
    ),
    -- Only workflow executions enter the durable queue.
    CONSTRAINT runs_execution_grain_check CHECK (
      trigger_source IS NOT NULL AND trigger_source IN ('request', 'schedule', 'event', 'manual')
      AND wiring_id IS NOT NULL AND wiring_id <> ''
      AND wiring_version IS NOT NULL AND wiring_version > 0
      AND wiring_hash IS NOT NULL AND wiring_hash ~ '^sha256:[0-9a-f]{64}$'
      AND execution_principal_id IS NOT NULL
      AND execution_role IS NOT NULL AND execution_role <> ''
      AND invoked_by IS NOT NULL AND invoked_by <> ''
      AND ((trigger_source IN ('request', 'manual') AND invoked_by = trigger_source AND invoked_by_user_id IS NOT NULL)
        OR (trigger_source = 'schedule' AND invoked_by = 'schedule' AND invoked_by_user_id IS NULL)
        OR trigger_source = 'event')
    ),
    PRIMARY KEY (run_id)
);
-- At-least-once: a redelivered trigger with the same key collapses to one run.
CREATE UNIQUE INDEX runs_idempotency ON wamn_run.runs
    (environment, package_id, wiring_id, wiring_version, trigger_source, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
-- History listing and trusted CDC event-causation traversal.
CREATE INDEX runs_event_root ON wamn_run.runs (event_root_run_id)
    WHERE event_root_run_id IS NOT NULL;
ALTER TABLE wamn_run.runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.runs FORCE ROW LEVEL SECURITY;
CREATE POLICY runs_identity ON wamn_run.runs
    TO wamn_app, wamn_query
    USING ((SELECT wamn_authority.current_role()) IS NOT NULL)
    WITH CHECK ((SELECT wamn_authority.current_role()) IS NOT NULL);
CREATE POLICY runs_platform ON wamn_run.runs
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING ((SELECT wamn_authority.current_role()) IS NOT NULL)
    WITH CHECK ((SELECT wamn_authority.current_role()) IS NOT NULL);

CREATE TRIGGER runs_event_lineage_immutable
BEFORE UPDATE OF event_source_run_id, event_root_run_id, event_depth
ON wamn_run.runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_event_lineage_immutable();
-- The guard is column-targeted, so the lease-time record columns must be named
-- here or the transition arm never fires for them.
CREATE TRIGGER runs_admission_pins_immutable
BEFORE UPDATE OF package_id, environment,
                 wiring_id, wiring_version,
                 wiring_hash, trigger_source,
                 execution_principal_id, execution_role, invoked_by, invoked_by_user_id
ON wamn_run.runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_run_admission_pins_immutable();
CREATE TRIGGER runs_terminal_delete_only
BEFORE DELETE ON wamn_run.runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_terminal_run_delete();
-- Application SQL reads run history. Platform work also prunes terminal runs;
-- runs_terminal_delete_only keeps active runs from being deleted.
REVOKE ALL PRIVILEGES ON TABLE wamn_run.runs
    FROM PUBLIC, wamn_app, wamn_platform;
GRANT SELECT ON wamn_run.runs TO wamn_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.runs TO wamn_platform;
