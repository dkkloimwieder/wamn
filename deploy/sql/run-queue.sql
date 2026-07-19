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
-- below) so `partitioned(key)` runs dispatch in-order per key across replicas,
-- plus the trigger dispatcher's OUTBOX (the `outbox` table below — D4: row
-- events are outbox rows polled by the dispatcher; LISTEN/NOTIFY removed).
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

-- ---------------------------------------------------------------------------
-- outbox: one row per durable row event (D4). A PRODUCER inserts the event row
-- in ITS OWN transaction — "outbox insert and enqueue can share a transaction
-- with user writes" (5.14) — so an event is durable iff the write it announces
-- is; in production the 3.2-emitted per-table trigger or the application write
-- path does the insert (crates/wamn-run-queue `outbox_insert_sql`). The trigger
-- DISPATCHER polls pending rows oldest-first with FOR UPDATE SKIP LOCKED
-- (`outbox_poll_sql`), fires one run per (matching row-event flow x row) via the
-- write-ahead + enqueue co-transaction, and acks (`outbox_ack_sql`,
-- dispatched_at) IN THAT SAME transaction: a crash before commit redelivers the
-- batch and retracts its enqueues atomically, and the deterministic run ids
-- ({flow}:outbox:{seq}) collapse the redelivery to no-ops. A row no flow is
-- registered on is acked as consumed-with-no-op (an unmatched backlog must not
-- pin the oldest-first poll window) — EXCEPT rows whose (table_name, event)
-- belongs to an ACTIVE flow the dispatcher could not parse/validate (a version
-- skew): those are HELD pending, so a skipped flow degrades to delayed
-- delivery, never silent event loss. `seq` is the identity and the poll's
-- oldest-first order; it is NOT a cross-replica dispatch-order guarantee
-- (SKIP LOCKED batches commit independently and outbox runs enqueue
-- unpartitioned — per-key ordering is the 5.11 `partition_key` seam). Acked
-- rows are short-lived audit history: the dispatcher's maintenance step prunes
-- them past a retention window (`outbox_prune_sql`, batch-bounded DELETE;
-- default 7 days — generous enough for forensics) so the outbox stays bounded.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.outbox (
    tenant_id     text NOT NULL CHECK (tenant_id <> ''),
    seq           bigint GENERATED ALWAYS AS IDENTITY,
    table_name    text NOT NULL,
    event         text NOT NULL CHECK (event IN ('insert', 'update', 'delete')),
    payload       jsonb,
    created_at    timestamptz NOT NULL DEFAULT now(),
    dispatched_at timestamptz,
    PRIMARY KEY (tenant_id, seq)
);
-- The poll scan: pending rows only, oldest-first.
CREATE INDEX outbox_pending ON wamn_run.outbox (tenant_id, seq)
    WHERE dispatched_at IS NULL;
ALTER TABLE wamn_run.outbox ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.outbox FORCE ROW LEVEL SECURITY;
CREATE POLICY outbox_tenant ON wamn_run.outbox
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.outbox TO wamn_app;
