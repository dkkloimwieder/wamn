-- Run-queue storage schema (5.14). The durable dispatch queue that co-transacts
-- with the 5.7 run state: `run_queue` (one row per run waiting to be, or being,
-- dispatched). Postgres owns durability (`FOR UPDATE SKIP LOCKED` claim + run
-- state, one durability domain — D3); NATS-core carries fire-and-forget doorbells
-- (a hint per enqueue) with a slow reconciliation sweep for lost hints; a
-- run-claim lease reclaims a dead replica's work. The claim/lease/janitor LOGIC
-- lives in crates/wamn-run-queue (pure); this file is the shape it reads and the
-- driver (crates/wamn-host queuebench / the dispatcher) writes.
--
-- STANDALONE ARTIFACT, ADDITIVE to deploy/sql/run-state.sql: same convention as
-- run-state.sql / catalog-schema.sql — deliberately NOT included by
-- deploy/sql/postgres-init.sql. Assumes deploy/sql/run-state.sql has been applied first
-- (schema `wamn_run` + the `runs` table this FKs, and the `wamn_app` role). The
-- queuebench gate provisions an ephemeral schema clone of `runs` + `run_queue`
-- (crates/wamn-host/src/queuebench.rs) rather than touching this production schema.
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
-- reconciliation, plus PER-PARTITION OWNERSHIP (the `partition_owner` lease table
-- below) so `partitioned(key)` runs dispatch in-order per key across replicas.
-- (Row events are the D19 v3 event plane's — CDC reader → JetStream →
-- materializer; the outbox table + poller were torn down at l5i9.19.)
-- Unpartitioned runs (`partition_key IS NULL`) claim globally in `available_at`
-- order; a run with a `partition_key` is dispatched only through the partition
-- path (crates/wamn-run-queue `acquire_partitions_sql` + `claim_partition_head_sql`),
-- under the row's `partition_policy` (D20: 'blocking' default / 'leapfrog' opt-in).
-- `priority` remains reserved (default 0).

-- ---------------------------------------------------------------------------
-- run_queue: one row per pending/in-flight run. `available_at` gates visibility
-- (future = a delayed/parked/backed-off run); a live `lease_expires_at` marks a
-- row a replica currently owns, and once it expires another replica may reclaim
-- it (crash-safe failover). `attempts` counts CRASH EVIDENCE only: a claim bumps
-- it iff it reclaims an expired lease (the prior owner died holding the run) — a
-- first claim and a park->wake re-claim (park releases the lease) are free, so a
-- flow may park unboundedly without spending redelivery budget. Once `attempts`
-- reaches `max_attempts` and the lease is long expired, the janitor marks the run
-- `infrastructure-failure` and removes the row — EXCEPT a 'blocking'-policy
-- partitioned row (D20): that row stays and WEDGES its key (operator release),
-- since reaping it would silently reorder a flow that opted into strict ordering.
-- The FK to `runs` ON DELETE CASCADE
-- ties the claim machinery to the run's immutable history. Status/lifecycle live
-- on `runs` (5.7) — the queue is the claim/lease layer, not a second run-state.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.run_queue (
    tenant_id        text NOT NULL CHECK (tenant_id <> ''),
    run_id           text NOT NULL,
    partition_key    text,
    -- D20: the key's head-unavailability policy, materialized at enqueue from the
    -- flow's declaration (inert when partition_key IS NULL). 'blocking' (default):
    -- a backed-off/parked/exhausted head still blocks its key — stream order is
    -- (enqueued_at, run_id), stamped once, never moved by a park (available_at is).
    -- 'leapfrog' (opt-in): a later ready run may overtake an unavailable head.
    partition_policy text NOT NULL DEFAULT 'blocking' CHECK (partition_policy IN ('blocking', 'leapfrog')),
    priority         int  NOT NULL DEFAULT 0,
    available_at     timestamptz NOT NULL DEFAULT now(),
    -- D19 §5 / E4: the per-flow monotone stream position that CDC event runs are
    -- keyed by (run_id = <flow>:evt:<stream_seq>). Carried AHEAD of run_id in the
    -- claim ordering key so evt runs dispatch by NUMERIC stream position, never
    -- lexical run-id order (f1:evt:10 must not precede f1:evt:9 — the R6/D20
    -- corruption class, arriving through a string comparison). 0 for every non-CDC
    -- enqueue today (the not-yet-built materializer passes real values); a uniform
    -- 0 makes the tiebreak inert, so today's dispatch order is byte-for-byte unchanged.
    stream_seq       bigint NOT NULL DEFAULT 0,
    lease_owner      text,
    lease_expires_at timestamptz,
    attempts         int  NOT NULL DEFAULT 0,
    max_attempts     int  NOT NULL DEFAULT 20,
    enqueued_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, run_id),
    FOREIGN KEY (tenant_id, run_id) REFERENCES wamn_run.runs (tenant_id, run_id) ON DELETE CASCADE
);
-- The claim scan: visible rows in dispatch order, filtered on lease liveness.
-- `stream_seq` sits in the ordering prefix (ahead of run_id, E4) so the claim's
-- `ORDER BY available_at, stream_seq, run_id` is index-supported; `lease_expires_at`
-- stays as the trailing in-index filter column it always was.
CREATE INDEX run_queue_claimable ON wamn_run.run_queue (tenant_id, available_at, stream_seq, lease_expires_at);
-- Per-partition ownership: the acquire candidate scan (distinct partition keys with
-- a claimable run) and the head-of-partition claim both key on (tenant, partition).
CREATE INDEX run_queue_partition ON wamn_run.run_queue (tenant_id, partition_key)
    WHERE partition_key IS NOT NULL;
ALTER TABLE wamn_run.run_queue ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.run_queue FORCE ROW LEVEL SECURITY;
CREATE POLICY run_queue_tenant ON wamn_run.run_queue
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.run_queue TO wamn_app;

-- ---------------------------------------------------------------------------
-- partition_owner: one lease per (tenant, partition_key). A runner replica leases a
-- partition and, while the lease is live, is the ONLY replica that dispatches that
-- key's runs (claiming them head-first, one in flight at a time — see
-- crates/wamn-run-queue `partition`), so ordering within the key is preserved under
-- horizontal scaling. When the owner dies the lease expires and another replica
-- reacquires the whole key and continues in order (crash-safe failover). This is a
-- coarse coordination row, not run state: it is NOT FK'd to run_queue (partition_key
-- is not unique there) and is garbage-collected when the partition drains
-- (`gc_orphan_partitions_sql`). The run lifecycle stays on `runs` (5.7); the
-- per-run lease stays on run_queue above.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.partition_owner (
    tenant_id        text NOT NULL CHECK (tenant_id <> ''),
    partition_key    text NOT NULL,
    lease_owner      text NOT NULL,
    lease_expires_at timestamptz NOT NULL,
    acquired_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, partition_key)
);
ALTER TABLE wamn_run.partition_owner ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.partition_owner FORCE ROW LEVEL SECURITY;
CREATE POLICY partition_owner_tenant ON wamn_run.partition_owner
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.partition_owner TO wamn_app;
