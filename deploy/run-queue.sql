-- Run-queue storage schema (5.14). The durable dispatch queue that co-transacts
-- with the 5.7 run state: `run_queue` (one row per run waiting to be, or being,
-- dispatched). Postgres owns durability (`FOR UPDATE SKIP LOCKED` claim + run
-- state, one durability domain — D3); NATS-core carries fire-and-forget doorbells
-- (a hint per enqueue) with a slow reconciliation sweep for lost hints; a
-- run-claim lease reclaims a dead replica's work. The claim/lease/janitor LOGIC
-- lives in crates/wamn-run-queue (pure); this file is the shape it reads and the
-- driver (crates/wamn-host queuebench / the dispatcher) writes.
--
-- STANDALONE ARTIFACT, ADDITIVE to deploy/run-state.sql: same convention as
-- run-state.sql / catalog-schema.sql — deliberately NOT included by
-- deploy/postgres-init.sql. Assumes deploy/run-state.sql has been applied first
-- (schema `wamn_run` + the `runs` table this FKs, and the `wamn_app` role). The
-- queuebench gate provisions an ephemeral schema clone of `runs` + `run_queue`
-- (crates/wamn-host/src/queuebench.rs) rather than touching this production schema.
--
-- Security shape mirrors the rest of the platform (runs/node_runs, s2/s3, catalog):
-- tenant separation purely via the `app.tenant` claim the wamn:postgres plugin
-- injects with SET LOCAL. FORCE RLS keyed on current_setting('app.tenant', true),
-- NULL (=> zero rows) when no claim was injected.
--
-- SCOPE (walking skeleton): the SKIP LOCKED queue + write-ahead + single-owner
-- leases + janitor + reconciliation. `partition_key` + `priority` are reserved
-- (nullable / default) for the deferred per-partition-ownership follow-up; the
-- skeleton claims globally in `available_at` order.

-- ---------------------------------------------------------------------------
-- run_queue: one row per pending/in-flight run. `available_at` gates visibility
-- (future = a delayed/parked/backed-off run); a live `lease_expires_at` marks a
-- row a replica currently owns, and once it expires another replica may reclaim
-- it (crash-safe failover). `attempts` bumps on every claim; once it reaches
-- `max_attempts` and the lease is long expired, the janitor marks the run
-- `infrastructure-failure` and removes the row. The FK to `runs` ON DELETE CASCADE
-- ties the claim machinery to the run's immutable history. Status/lifecycle live
-- on `runs` (5.7) — the queue is the claim/lease layer, not a second run-state.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.run_queue (
    tenant_id        text NOT NULL,
    run_id           text NOT NULL,
    partition_key    text,
    priority         int  NOT NULL DEFAULT 0,
    available_at     timestamptz NOT NULL DEFAULT now(),
    lease_owner      text,
    lease_expires_at timestamptz,
    attempts         int  NOT NULL DEFAULT 0,
    max_attempts     int  NOT NULL DEFAULT 20,
    enqueued_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, run_id),
    FOREIGN KEY (tenant_id, run_id) REFERENCES wamn_run.runs (tenant_id, run_id) ON DELETE CASCADE
);
-- The claim scan: visible rows in dispatch order, filtered on lease liveness.
CREATE INDEX run_queue_claimable ON wamn_run.run_queue (tenant_id, available_at, lease_expires_at);
-- Reserved for per-partition ownership (deferred 5.14 scaling follow-up).
CREATE INDEX run_queue_partition ON wamn_run.run_queue (tenant_id, partition_key)
    WHERE partition_key IS NOT NULL;
ALTER TABLE wamn_run.run_queue ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.run_queue FORCE ROW LEVEL SECURITY;
CREATE POLICY run_queue_tenant ON wamn_run.run_queue
    USING (tenant_id = current_setting('app.tenant', true))
    WITH CHECK (tenant_id = current_setting('app.tenant', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.run_queue TO wamn_app;
