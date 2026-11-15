-- Run-queue storage schema (5.14). The durable dispatch queue that co-transacts
-- with the 5.7 run state: `run_queue` (one row per run waiting to be, or being,
-- dispatched). Postgres owns durability (`FOR UPDATE SKIP LOCKED` lease + run
-- state share one durability domain). The host polls due work and leases rows.
-- A run lease lets another replica recover abandoned work.
-- Run persistence lives in crates/run-state.
-- The host and workflow use this schema to lease and enqueue work.
--
-- STANDALONE ARTIFACT, ADDITIVE to deploy/sql/run-state.sql: same convention as
-- run-state.sql / catalog-schema.sql — deliberately NOT included by
-- deploy/sql/postgres-init.sql. Assumes deploy/sql/run-state.sql has been applied first
-- (schema `wamn_run` + the `runs` table this FKs, and the `wamn_app` role). The
-- Project-environment provisioning and schema reconciliation apply this artifact
-- after `run-state.sql`; it is not an independent durability domain.
--
-- Only host roles can read or write the queue. A project database holds one
-- project. FORCE RLS requires the protected caller row, and table grants
-- select the host roles that can use it.
--
-- TARGET: the SKIP LOCKED queue + write-ahead + single-owner leases + janitor +
-- reconciliation. Every row shares one project-global FIFO ordered by
-- `(available_at, run_id)`. Runs have no event ordering guarantee.
-- The CDC reader publishes row events through JetStream to workflow admission.

-- ---------------------------------------------------------------------------
-- run_queue: one row per pending/in-flight run. `available_at` conditions visibility
-- (future = a queue-parked/backed-off run); a live `lease_expires_at` marks a
-- row a replica currently owns, and once it expires another replica may recover
-- it (crash-safe failover). `attempts` counts expired lease recovers and releases
-- for unavailable credentials or pools. Operator parks and wakes are free.
-- Both causes share `max_attempts`. A credential release at the limit fails the
-- run with `retry-exhausted`. An exhausted expired lease ends the run with
-- `infrastructure-failure` after the janitor grace period. Credential releases
-- back off from one second, doubled per attempt, with up to 20 percent jitter.
-- `queue::MAX_RETRY_BACKOFF_MS` caps the full delay at five minutes.
-- The FK to `runs` ON DELETE CASCADE
-- ties the lease machinery to the run's immutable history. Status/lifecycle live
-- on `runs` (5.7) — the queue is the lease layer, not a second run-state.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.run_queue (
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    project_env_key          text NOT NULL CHECK (project_env_key <> ''),
    run_id           text NOT NULL,
    available_at     timestamptz NOT NULL DEFAULT now(),
    lease_owner      text,
    lease_expires_at timestamptz,
    -- Queue-owned executor authority (§9.5). Every lease/recover increments the
    -- generation; executor mutations join on owner + generation.
    lease_generation bigint NOT NULL DEFAULT 0 CHECK (lease_generation >= 0),
    attempts         int  NOT NULL DEFAULT 0,
    -- Backoff cap: crates/run-state/src/queue/mod.rs.
    max_attempts     int  NOT NULL DEFAULT 20,
    enqueued_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_env_key, run_id),
    FOREIGN KEY (project_env_key, run_id) REFERENCES wamn_run.runs (project_env_key, run_id) ON DELETE CASCADE
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON wamn_run.run_queue
    FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row(
        'created_at', 'created_by', 'updated_at', 'updated_by');
-- The lease scan: visible rows in dispatch order, filtered on lease liveness.
-- `run_id` completes the ordering prefix, so the exact
-- `ORDER BY available_at, run_id` is index-supported;
-- `lease_expires_at` remains the trailing in-index filter column.
CREATE INDEX run_queue_leasable ON wamn_run.run_queue
    (project_env_key, available_at, run_id, lease_expires_at);
ALTER TABLE wamn_run.run_queue ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.run_queue FORCE ROW LEVEL SECURITY;
CREATE POLICY run_queue_project_env_key ON wamn_run.run_queue
    TO wamn_platform
    USING ((SELECT wamn_authority.current_role()) IS NOT NULL)
    WITH CHECK ((SELECT wamn_authority.current_role()) IS NOT NULL);
REVOKE ALL PRIVILEGES ON TABLE wamn_run.run_queue
    FROM PUBLIC, wamn_app;
