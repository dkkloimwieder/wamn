-- Run-queue storage schema (5.14). The durable dispatch queue that co-transacts
-- with the 5.7 run state: `run_queue` (one row per run waiting to be, or being,
-- dispatched). Postgres owns durability (`FOR UPDATE SKIP LOCKED` claim + run
-- state, one durability domain — D3); NATS-core carries fire-and-forget doorbells
-- (a hint per enqueue) with a slow reconciliation sweep for lost hints; a
-- run-claim lease reclaims a dead replica's work. The claim/lease/janitor LOGIC
-- lives in crates/execution/run-state/src/queue (pure); this file is the shape
-- used by the host-owned production claim/admission path and materializer
-- enqueue. Dispatcher reconciliation reads due/depth state but does not write it.
--
-- STANDALONE ARTIFACT, ADDITIVE to deploy/sql/run-state.sql: same convention as
-- run-state.sql / catalog-schema.sql — deliberately NOT included by
-- deploy/sql/postgres-init.sql. Assumes deploy/sql/run-state.sql has been applied first
-- (schema `wamn_run` + the `runs` table this FKs, and the `wamn_app` role). The
-- Project-environment provisioning and schema reconciliation apply this artifact
-- after `run-state.sql`; it is not an independent durability domain.
--
-- Security shape mirrors the rest of the platform (runs/node_runs, s2/s3, catalog):
-- tenant separation purely via the `app.tenant` claim the wamn:postgres plugin
-- injects with SET LOCAL. FORCE RLS keyed on
-- NULLIF(current_setting('app.tenant', true), ''), NULL (=> zero rows) when no
-- claim was injected — PG resets a custom GUC to '' (not NULL) after SET LOCAL,
-- and CHECK (tenant_id <> '') forbids a ''-tenant row, so an empty claim
-- matches nothing structurally.
--
-- SCOPE: the SKIP LOCKED queue + write-ahead + single-owner leases + janitor +
-- reconciliation. Every row shares one tenant-global FIFO ordered by
-- `(available_at, stream_seq, run_id)`; `priority` remains reserved (default 0).
-- (Row events are the D19 v3 event plane's — CDC reader → JetStream →
-- materializer; the outbox table + poller were torn down at l5i9.19.)

-- ---------------------------------------------------------------------------
-- run_queue: one row per pending/in-flight run. `available_at` gates visibility
-- (future = a queue-parked/backed-off run); a live `lease_expires_at` marks a
-- row a replica currently owns, and once it expires another replica may reclaim
-- it (crash-safe failover). `attempts` counts CRASH EVIDENCE only: a claim bumps
-- it iff it reclaims an expired lease (the prior owner died holding the run) — a
-- first claim and a queue-park->wake re-claim (the queue park releases the lease)
-- are free, so bounded-retry waits do not spend redelivery budget. This queue
-- eligibility operation is distinct from node execution state. Once `attempts`
-- reaches `max_attempts` and the lease is long expired, the janitor marks the run
-- `infrastructure-failure` and removes the row. The FK to `runs` ON DELETE CASCADE
-- ties the claim machinery to the run's immutable history. Status/lifecycle live
-- on `runs` (5.7) — the queue is the claim/lease layer, not a second run-state.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.run_queue (
    tenant_id        text NOT NULL CHECK (tenant_id <> ''),
    run_id           text NOT NULL,
    priority         int  NOT NULL DEFAULT 0,
    available_at     timestamptz NOT NULL DEFAULT now(),
    -- D19 §5 / E4: the per-flow monotone stream position that CDC event runs are
    -- keyed by (run_id = <flow>:evt:<stream_seq>). Carried AHEAD of run_id in the
    -- claim ordering key so evt runs dispatch by NUMERIC stream position, never
    -- lexical run-id order (f1:evt:10 must not precede f1:evt:9 — the R6/D20
    -- corruption class, arriving through a string comparison). 0 for every non-CDC
    -- enqueue; the materializer carries the real CDC stream position. A uniform 0
    -- makes this tiebreak inert for other admission paths.
    stream_seq       bigint NOT NULL DEFAULT 0,
    lease_owner      text,
    lease_expires_at timestamptz,
    -- Queue-owned executor authority (§9.5). Every claim/reclaim increments the
    -- generation; executor mutations join on owner + generation.
    lease_generation bigint NOT NULL DEFAULT 0 CHECK (lease_generation >= 0),
    attempts         int  NOT NULL DEFAULT 0,
    max_attempts     int  NOT NULL DEFAULT 20,
    enqueued_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, run_id),
    FOREIGN KEY (tenant_id, run_id) REFERENCES wamn_run.runs (tenant_id, run_id) ON DELETE CASCADE
);
-- The claim scan: visible rows in dispatch order, filtered on lease liveness.
-- `stream_seq` and `run_id` complete the ordering prefix, so the exact
-- `ORDER BY available_at, stream_seq, run_id` is index-supported;
-- `lease_expires_at` remains the trailing in-index filter column.
CREATE INDEX run_queue_claimable ON wamn_run.run_queue
    (tenant_id, available_at, stream_seq, run_id, lease_expires_at);
ALTER TABLE wamn_run.run_queue ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.run_queue FORCE ROW LEVEL SECURITY;
CREATE POLICY run_queue_tenant ON wamn_run.run_queue
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
REVOKE ALL PRIVILEGES ON TABLE wamn_run.run_queue FROM PUBLIC, wamn_effect_writer;
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.run_queue TO wamn_app;
-- The private effect writer rechecks only exact live queue authority after
-- acquiring the shared tenant/run advisory fence. `lease_generation` prevents
-- an owner/expiry ABA match; this remains read-only fence evidence.
GRANT SELECT (tenant_id, run_id, lease_owner, lease_expires_at, lease_generation)
    ON wamn_run.run_queue TO wamn_effect_writer;
