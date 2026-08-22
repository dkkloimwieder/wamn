-- Run-state storage schema (5.7). The production tables that PERSIST flow
-- execution: `runs` (one row per execution) and `node_runs` (one row per node
-- execution). This is the durable, queryable record behind run history, at-
-- least-once execution and immutable run/node history. The pure engine
-- (crates/execution/flow-engine, 5.2) is a single-shot in-memory reducer; these
-- tables are the facts the driver (components/execution/flowrunner) writes.
--
-- STANDALONE ARTIFACT: deliberately NOT included by deploy/sql/postgres-init.sql, the
-- same convention as deploy/sql/catalog-schema.sql (3.1/3.4/3.5/3.6). The S3/S6 gate
-- fixtures carry their own `runs`/`node_runs` copies (postgres-init.sql schema
-- `s3`) so flowbench exercises the rewired runner; this file is the production schema and the
-- target of the crate's live-apply gate. Assumes pre-existing `wamn_app`,
-- `wamn_scenario_author`, stable `wamn_effect_writer`, and stable
-- `wamn_run_projection_writer` ACL roles. Role and
-- scoped LOGIN credential-generation lifecycle is provisioning-owned; this
-- artifact grants the stable role ledger append/read authority plus only the
-- narrow run columns needed for its fenced runnable-state recheck.
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
-- their own table; the run-level I/O CAPTURE policy (9.6) is fixed at admission
-- and full capture fills scrubbed `input_json`/`output_json` plus the output
-- size and optional hash; the content-addressed payload BYTE store (5.10) is
-- pointed at by `input_ref`/`output_ref`.

CREATE SCHEMA IF NOT EXISTS wamn_run AUTHORIZATION CURRENT_USER;
REVOKE ALL PRIVILEGES ON SCHEMA wamn_run
    FROM PUBLIC, wamn_effect_writer, wamn_run_projection_writer;
GRANT USAGE ON SCHEMA wamn_run TO wamn_app;
GRANT USAGE ON SCHEMA wamn_run TO wamn_scenario_author;
GRANT USAGE ON SCHEMA wamn_run TO wamn_effect_writer;
GRANT USAGE ON SCHEMA wamn_run TO wamn_run_projection_writer;

-- Producer roles cannot name `runs.durability_class` in their INSERT grants.
-- This invoker-rights trigger therefore performs the only admission-time
-- selection, from the project-local projection below.
CREATE FUNCTION wamn_run.pin_run_durability_class()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    projected_environment text;
    projected_class text;
BEGIN
    SELECT policy.expected_environment, policy.durability_class
      INTO projected_environment, projected_class
      FROM wamn_run.environment_policies AS policy
     WHERE policy.tenant_id = NEW.tenant_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'environment-policy-not-converged';
    END IF;
    IF NEW.environment IS DISTINCT FROM projected_environment THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'environment-policy-environment-mismatch';
    END IF;
    NEW.durability_class := projected_class;
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION wamn_run.pin_run_durability_class() FROM PUBLIC;

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

-- Effect attempt, dispatch, and outcome facts are immutable even for their
-- owning role; retention requires a future explicit ledger protocol.
CREATE FUNCTION wamn_run.reject_immutable_effect_fact_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = 'effect-fact-immutable';
END
$$;
REVOKE ALL ON FUNCTION wamn_run.reject_immutable_effect_fact_change() FROM PUBLIC;

-- Operator actions are immutable evidence and never follow run-history
-- pruning. Only the schema-owning project-admin path can append them.
CREATE FUNCTION wamn_run.reject_immutable_operator_run_action_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = 'operator-run-action-immutable';
END
$$;
REVOKE ALL ON FUNCTION wamn_run.reject_immutable_operator_run_action_change()
    FROM PUBLIC;

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

-- Admission pins never change. The claim-time release record is write-once PER
-- CLAIM ATTEMPT, not per run-eternity: it names THE RELEASE OF THE CLAIM
-- CURRENTLY EXECUTING THIS RUN (wamn-0h0g.13.55). It is NULL on the admitted
-- row, the claiming worker writes its own pod identity, and EVERY arm that
-- REOPENS CLAIMABILITY — the classifier's pre-effect reclaim and the queue park
-- that releases a lease — clears it again so the next claim records afresh. The
-- guard is therefore TRANSITION-CONSTRAINED:
--     NULL  -> value    permitted  (a claim records this attempt's pod)
--     value -> NULL     permitted  (an arm reopens claimability)
--     value -> value'   REFUSED always
-- A trigger cannot see WHICH statement is updating the row, so the erasure arm
-- does not try to name its caller; it encodes the property that makes the
-- erasure safe. Two legs remain, and each defends a distinct thing:
--   * STILL RUNNABLE. A terminal run keeps the audit link to the plan hashes it
--     finished under. Nothing reopens a terminal run's claimability, so nothing
--     needs to erase it.
--   * NO IMMUTABLE EFFECT ATTEMPT, ON A `durable` RUN. An attributed effect
--     names the release that fired it, and that link is never rewritten out
--     from under it. This is the leg that refuses a mid-effect release rewrite;
--     a run carrying an attempt is classified terminal effect-uncertain by its
--     next claim and never re-executes under a second release.
--     THE CLASS PREDICATE IS LOAD-BEARING, NOT DECORATION (wamn-0h0g.20.2).
--     `queue/sql.rs` `park_sql` carries the SAME `durability_class = 'durable'`
--     predicate on the SAME `EXISTS(effect_attempts)`, and the two must move
--     together in BOTH directions. Gate only the park and this guard refuses
--     the erasure the park attempts, aborting the park. Gate only this guard
--     and a `standard` run keeps a release record across a park it should have
--     cleared, so a waking pod on a different release refuses at the grant —
--     and because a released lease is not crash evidence the refusal spends no
--     budget, the janitor can never reap it, and the run is its tenant's FIFO
--     head forever. That is exactly wamn-0h0g.15.82, resurrected by a half-
--     applied class gate.
-- The `node_runs` leg that stood beside them was dropped by wamn-0h0g.15.82. It
-- encoded the OLD contract, "the release this RUN executed under"; under the
-- redefined contract an executed node is a HISTORY fact, not a current-claim
-- fact. Keeping it made the queue park the one claimability-reopening arm that
-- could not clear (a parked run has generally executed nodes), so a pod waking
-- that run on a different release refused — and because a released lease is not
-- crash evidence the run was then both unreapable and permanently its tenant's
-- FIFO head. What nodes 1..k ran under is a per-segment audit question whose
-- honest home is per-claim, and nothing is built for it here.
CREATE FUNCTION wamn_run.guard_run_admission_pins_immutable()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.catalog_id IS DISTINCT FROM OLD.catalog_id
       OR NEW.catalog_version IS DISTINCT FROM OLD.catalog_version
       OR NEW.environment IS DISTINCT FROM OLD.environment
       OR NEW.execution_bundle_hash IS DISTINCT FROM OLD.execution_bundle_hash
       OR NEW.capture_mode IS DISTINCT FROM OLD.capture_mode
       OR NEW.durability_class IS DISTINCT FROM OLD.durability_class THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'run-admission-pin-immutable';
    END IF;
    IF OLD.release_version IS NOT NULL OR OLD.manifest_digest IS NOT NULL THEN
        IF NEW.release_version IS NULL AND NEW.manifest_digest IS NULL THEN
            IF NEW.status NOT IN ('dispatched', 'running')
               OR EXISTS (SELECT 1 FROM wamn_run.effect_attempts AS effect
                           WHERE effect.tenant_id = OLD.tenant_id
                             AND effect.run_id = OLD.run_id
                             AND OLD.durability_class = 'durable') THEN
                RAISE EXCEPTION USING
                    ERRCODE = '55000',
                    MESSAGE = 'run-release-record-immutable';
            END IF;
        ELSIF NEW.release_version IS DISTINCT FROM OLD.release_version
           OR NEW.manifest_digest IS DISTINCT FROM OLD.manifest_digest THEN
            RAISE EXCEPTION USING
                ERRCODE = '55000',
                MESSAGE = 'run-release-record-immutable';
        END IF;
    END IF;
    RETURN NEW;
END
$$;

-- `wamn_app` retains DELETE only for tenant-scoped history pruning. The table
-- grant cannot express the prune statement's terminal-state predicate, so this
-- ordinary invoker-rights trigger makes that predicate caller-independent. An
-- `effect-uncertain` run is deliberately not terminal: its operator resolution
-- remains part of the durable audit floor and cannot be pruned as history
-- (wamn-0h0g.12.128).
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
-- by `reconcile-run-plane`; admission reads only this local relation and freezes
-- the selected class onto the run row, so policy changes affect future runs only.
-- A project database is constant for (org, project, env), so expected_environment
-- is verified, never selected as a second key. One tenant has one row; a missing
-- or mismatched projection refuses admission rather than inventing a decision.
CREATE TABLE wamn_run.environment_policies (
    tenant_id            text NOT NULL CHECK (tenant_id <> ''),
    expected_environment text NOT NULL CHECK (expected_environment <> ''),
    durability_class    text NOT NULL
        CONSTRAINT environment_policies_durability_class_check
        CHECK (durability_class IN ('standard', 'durable')),
    PRIMARY KEY (tenant_id)
);
ALTER TABLE wamn_run.environment_policies ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.environment_policies FORCE ROW LEVEL SECURITY;
CREATE POLICY environment_policies_tenant
ON wamn_run.environment_policies
FOR SELECT
USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
REVOKE ALL PRIVILEGES ON TABLE wamn_run.environment_policies
    FROM PUBLIC, wamn_app, wamn_scenario_author, wamn_effect_writer,
         wamn_run_projection_writer;
GRANT SELECT ON TABLE wamn_run.environment_policies TO wamn_app;
GRANT SELECT ON TABLE wamn_run.environment_policies TO wamn_scenario_author;

-- ---------------------------------------------------------------------------
-- runs: one row per flow execution. `input_json` is the admitted trigger
-- payload; `result_json` is the last node's output on
-- completion; `state_json` carries transient run state such as bounded-retry
-- scheduling and execution context. `idempotency_key` dedupes at-least-once
-- redelivery of the same trigger (a partial-unique index). `fail_kind` mirrors
-- the engine `FailKind` so history can
-- flag an upstream bug (`invalid-input`) apart from a terminal error or an
-- exhausted retry budget. Status values are exactly wamn_run_store::RunStatus
-- as_sql (tied to the crate by a drift-guard test).
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.runs (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    run_id          text NOT NULL,
    flow_id         text NOT NULL,
    flow_version    int  NOT NULL,
    catalog_id      text NOT NULL,
    catalog_version int NOT NULL,
    environment     text NOT NULL,
    execution_bundle_hash text NOT NULL,
    attachment_id   text,
    registration_id text,
    -- Trusted immutable CDC event ancestry is distinct from retired execution
    -- lineage. The immediate source lets admission verify the carried root/depth
    -- under tenant RLS.
    event_source_run_id text,
    event_root_run_id text,
    event_depth      int CHECK (event_depth BETWEEN 0 AND 16),
    status          text NOT NULL DEFAULT 'running'
        CHECK (status IN ('dispatched', 'running', 'completed', 'failed',
                          'infrastructure-failure', 'effect-uncertain')),
    trigger_source  text,
    capture_mode    text NOT NULL DEFAULT 'off'
        CONSTRAINT runs_capture_mode_check CHECK (capture_mode IN ('full', 'off')),
    -- The DURABILITY CLASS the run was admitted under (wamn-0h0g.20.1). The
    -- carrier is per-run; the SOURCE is the env/org policy consulted at
    -- admission, never a caller parameter — the same authority rationale that
    -- keeps `capture_mode` off the admission parameter list. `standard` is R2's
    -- default crash floor (plain lock-then-lease, no claim-time effect
    -- classification); `durable` re-enables the premium floor at path 3's
    -- claim. The default is FAIL-OPEN TO THE CHEAP TIER: an admission that
    -- omits the column (every admission today — the column is withheld from
    -- `wamn_app`'s INSERT set below) takes `standard`, never `durable`.
    durability_class text NOT NULL DEFAULT 'standard'
        CONSTRAINT runs_durability_class_check
        CHECK (durability_class IN ('standard', 'durable')),
    -- The claim-time release record. A run is NOT version-pinned at admission:
    -- it executes under the release its CLAIMING pod carries, and the worker
    -- writes that pod's own release identity here when it takes the lease,
    -- once per claim attempt, enforced by `guard_run_admission_pins_immutable`.
    -- Both are NULL on the admitted row and move together. `release_version`
    -- is the release (catalog) version; `manifest_digest` is the RFC 8785
    -- digest of that release's serving manifest, and therefore the audit link
    -- from the run to the plan hashes it executed.
    release_version int,
    manifest_digest text,
    input_json      jsonb,
    result_json     jsonb,
    state_json      jsonb,
    invocation_context jsonb NOT NULL DEFAULT '{}'::jsonb,
    admission_context_version text NOT NULL DEFAULT '0.1'
        CHECK (admission_context_version = '0.1'),
    platform_revision text NOT NULL DEFAULT 'legacy',
    idempotency_key text,
    caller_outcome_kind text
        CHECK (caller_outcome_kind IN ('responded', 'failed')),
    caller_outcome_json jsonb,
    caller_http_status int CHECK (caller_http_status BETWEEN 100 AND 599),
    caller_release_node_id text,
    caller_outcome_hash text,
    caller_released_at timestamptz,
    response_deadline_at timestamptz,
    run_deadline_at timestamptz,
    terminal_reason text,
    fail_kind       text CHECK (fail_kind IN ('terminal', 'retry-exhausted', 'invalid-input',
                                              'runaway-budget', 'effect-uncertain', 'depth-budget',
                                              'dispatch-budget', 'unresolvable-name',
                                              'hash-invalid-bytes', 'foreign-revision',
                                              'incompatible-contract', 'unbound-requirement')),
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (catalog_id <> ''
           AND catalog_version > 0
           AND environment <> ''
           AND execution_bundle_hash ~ '^sha256:[0-9a-f]{64}$'),
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
    CONSTRAINT runs_check6
        CHECK ((caller_released_at IS NULL) = (caller_outcome_kind IS NULL)),
    CONSTRAINT runs_check7
        CHECK (caller_outcome_kind IS NULL OR caller_outcome_json IS NOT NULL),
    CONSTRAINT runs_check8
        CHECK (caller_outcome_kind <> 'responded' OR caller_release_node_id IS NOT NULL),
    CONSTRAINT runs_check9
        CHECK (response_deadline_at IS NULL OR run_deadline_at IS NULL
               OR response_deadline_at <= run_deadline_at),
    CONSTRAINT runs_capture_mode_source_check CHECK (
      capture_mode <> 'full' OR trigger_source IS NOT DISTINCT FROM 'scenario-draft'
    ),
    -- Both `IS NOT NULL` conjuncts are load-bearing: without them a well-formed
    -- HALF pair makes this disjunct NULL rather than false, and a CHECK whose
    -- expression is NULL is SATISFIED — so `(7, NULL)` and `(NULL, <digest>)`
    -- were both accepted and the pair was not paired (wamn-0h0g.15.126).
    CONSTRAINT runs_release_record_check CHECK (
      (release_version IS NULL AND manifest_digest IS NULL)
      OR (release_version IS NOT NULL AND manifest_digest IS NOT NULL
          AND release_version > 0
          AND manifest_digest ~ '^sha256:[0-9a-f]{64}$')
    ),
    PRIMARY KEY (tenant_id, run_id),
    CONSTRAINT runs_release_fk
        FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.release_manifests (tenant_id, catalog_id, catalog_version),
    CONSTRAINT runs_execution_bundle_fk
        FOREIGN KEY (tenant_id, execution_bundle_hash)
        REFERENCES catalog.execution_bundles (tenant_id, execution_bundle_hash)
);
-- At-least-once: a redelivered trigger with the same key collapses to one run.
CREATE UNIQUE INDEX runs_idempotency ON wamn_run.runs (tenant_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
-- History listing and trusted CDC event-causation traversal.
CREATE INDEX runs_flow ON wamn_run.runs (tenant_id, flow_id, created_at);
CREATE INDEX runs_release ON wamn_run.runs (tenant_id, catalog_id, catalog_version);
CREATE INDEX runs_execution_bundle ON wamn_run.runs (tenant_id, execution_bundle_hash);
CREATE INDEX runs_event_root ON wamn_run.runs (tenant_id, event_root_run_id)
    WHERE event_root_run_id IS NOT NULL;
CREATE INDEX runs_response_deadline ON wamn_run.runs (tenant_id, response_deadline_at)
    WHERE caller_released_at IS NULL
      AND response_deadline_at IS NOT NULL
      AND status IN ('dispatched', 'running');
CREATE INDEX runs_run_deadline ON wamn_run.runs (tenant_id, run_deadline_at)
    WHERE run_deadline_at IS NOT NULL
      AND status IN ('dispatched', 'running');
ALTER TABLE wamn_run.runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.runs FORCE ROW LEVEL SECURITY;
CREATE POLICY runs_tenant ON wamn_run.runs
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));

CREATE TRIGGER runs_pin_durability_class
BEFORE INSERT ON wamn_run.runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.pin_run_durability_class();

CREATE TRIGGER runs_event_lineage_immutable
BEFORE UPDATE OF event_source_run_id, event_root_run_id, event_depth
ON wamn_run.runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_event_lineage_immutable();
-- The guard is column-scoped, so the claim-time record columns must be named
-- here or the transition arm never fires for them.
CREATE TRIGGER runs_admission_pins_immutable
BEFORE UPDATE OF catalog_id, catalog_version, environment, execution_bundle_hash, capture_mode,
                 durability_class, release_version, manifest_digest
ON wamn_run.runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_run_admission_pins_immutable();
CREATE TRIGGER runs_terminal_delete_only
BEFORE DELETE ON wamn_run.runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_terminal_run_delete();
-- The guest-visible application role may drive the existing run-state columns,
-- but it cannot author or mutate the admission-owned capture carrier, nor the
-- admission-owned durability-class carrier. Off-path admissions omit those
-- columns and take their fail-closed database defaults (`off` for capture;
-- `standard` — the CHEAP tier — for the class).
--
-- The two column lists below are RATIFIED SETS (wamn-0h0g.12.40), not "every
-- canonical column minus capture_mode". Each is the exact union of the columns
-- named by statements `wamn_app` actually executes: the INSERT set is the
-- callable admission's run insert (crates/execution/run-state/src/admission.rs
-- `admit_sql`), which subsumes every other app-role run insert; the UPDATE set
-- is the union of the run-plane's claim, park, release, and terminalize
-- statements (queue/sql.rs, transitions.rs). Columns whose only writer is the
-- management admission (`capture_mode`) or the project-admin operator verb are
-- DELIBERATELY ABSENT, as is `durability_class`.
-- Its only admission-time writer is `runs_pin_durability_class`, which resolves
-- the project-local system-policy projection; no producer statement may name
-- it. A column added to this table does NOT join
-- either set by default; see `repair_run_capture_privilege_sql`.
--
-- The UPDATE set is also what keeps `FOR UPDATE`/`FOR KEY SHARE` on `runs`
-- legal: PostgreSQL demands UPDATE on at least one column for any row-locking
-- clause, and the run plane locks this table in the claim and fence paths.
--
-- DELETE stays table-wide because `wamn-ctl prune-run-history` connects AS
-- `wamn_app` and needs it. `runs_terminal_delete_only` makes the statement's
-- terminal-only predicate caller-independent without adding a privileged role.
REVOKE ALL PRIVILEGES ON TABLE wamn_run.runs FROM PUBLIC, wamn_effect_writer;
GRANT SELECT, DELETE ON wamn_run.runs TO wamn_app;
GRANT INSERT (
    tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version,
    environment, execution_bundle_hash, attachment_id, registration_id,
    event_source_run_id, event_root_run_id, event_depth, status, trigger_source,
    input_json, invocation_context,
    admission_context_version, platform_revision, idempotency_key,
    response_deadline_at, run_deadline_at
), UPDATE (
    status, release_version, manifest_digest, result_json, state_json,
    caller_outcome_kind, caller_outcome_json,
    caller_http_status, caller_release_node_id, caller_outcome_hash,
    caller_released_at, terminal_reason, fail_kind, updated_at
) ON wamn_run.runs TO wamn_app;
GRANT SELECT ON wamn_run.runs TO wamn_scenario_author;
-- The private effect writer may only recheck that the fenced run still has
-- runnable state. Lease-generation authority remains outside this schema lane.
GRANT SELECT (tenant_id, run_id, status)
    ON wamn_run.runs TO wamn_effect_writer;

-- HTTP invocation idempotency ledger (§6.2). The identity is intentionally
-- definition-independent: reusing a client key after a definition change must
-- find the old admission and return `idempotency-scope-changed`, never create a
-- second run. The named unique constraint is mapped to the transitions module's
-- typed `duplicate-identity` refusal. Pure call-free admissions may omit a
-- client key; PostgreSQL's distinct NULLs make each such admission a new run.
CREATE TABLE wamn_run.invocation_admissions (
    tenant_id                  text NOT NULL CHECK (tenant_id <> ''),
    catalog_id                 text NOT NULL,
    environment                text NOT NULL,
    attachment_id              text NOT NULL,
    definition_hash            text NOT NULL,
    principal_digest           text NOT NULL,
    client_key_digest          text,
    client_request_fingerprint text NOT NULL,
    admitted_catalog_version   bigint NOT NULL,
    admitted_flow_version      int NOT NULL,
    run_id                     text NOT NULL,
    created_at                 timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT invocation_admissions_identity UNIQUE
        (tenant_id, catalog_id, environment, attachment_id,
         principal_digest, client_key_digest),
    CONSTRAINT invocation_admissions_run_fk FOREIGN KEY (tenant_id, run_id)
        REFERENCES wamn_run.runs (tenant_id, run_id) ON DELETE CASCADE
        DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX invocation_admissions_run
    ON wamn_run.invocation_admissions (tenant_id, run_id);
ALTER TABLE wamn_run.invocation_admissions ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.invocation_admissions FORCE ROW LEVEL SECURITY;
CREATE POLICY invocation_admissions_tenant ON wamn_run.invocation_admissions
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
-- The ledger is APPEND-ONLY to the guest-visible role (wamn-0h0g.12.41). The
-- only production write is the callable admission's `ON CONFLICT DO NOTHING`
-- insert; nothing updates a row and nothing deletes one.
--
-- `UPDATE (tenant_id)` is NOT a rewrite authority — it is the minimum
-- PostgreSQL demands for the `FOR KEY SHARE OF a` in admission.rs, which
-- requires UPDATE on at least one column for ANY row-locking clause. It is safe
-- on `tenant_id` specifically because this table is FORCE ROW LEVEL SECURITY
-- and `invocation_admissions_tenant`'s WITH CHECK admits only the value the
-- USING clause already required to see the row, so the sole writable column can
-- only ever be rewritten to the value it already holds.
--
-- No DELETE grant is needed for the `ON DELETE CASCADE` from `runs`: a
-- referential-integrity action runs as the REFERENCING table's owner, not as
-- the deleting role, so pruning a run still removes its admission.
GRANT SELECT, INSERT ON wamn_run.invocation_admissions TO wamn_app;
GRANT UPDATE (tenant_id) ON wamn_run.invocation_admissions TO wamn_app;

-- ---------------------------------------------------------------------------
-- node_runs: one row per framed node execution.
-- The occurrence identity is (tenant_id, run_id, frame_id, local_node_id, occurrence):
-- `occurrence` disambiguates a node the frame LOOPS through (0 = first visit).
-- Frameless/call-free executions use the root frame (frame_id 0, no parent or
-- call site) with current_plan_hash = runs.execution_bundle_hash.
-- `seq` preserves execution order for history reads. `input_json` and
-- `output_json` carry capture facts when the run's admission mode allows them.
-- The `*_ref` columns are RESERVED nullable seams
-- for 5.10 (payload byte store). Capture policy belongs to the run admission
-- row and is not duplicated per node; full capture stores only scrubbed input,
-- output, output size, and the optional output hash.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.node_runs (
    tenant_id     text NOT NULL CHECK (tenant_id <> ''),
    run_id        text NOT NULL,
    frame_id      bigint NOT NULL DEFAULT 0,
    parent_frame_id bigint,
    call_site_id  text,
    current_plan_hash text NOT NULL,
    local_node_id text NOT NULL,
    occurrence    int  NOT NULL DEFAULT 0,
    seq           bigint NOT NULL,
    status        text NOT NULL
        CHECK (status IN ('started', 'success', 'error')),
    output_port   text,
    output_json   jsonb,
    input_json    jsonb,
    error_kind    text CHECK (error_kind IN ('retryable', 'rate-limited', 'terminal',
                                            'invalid-input')),
    error_detail  jsonb,
    -- Reserved seams (5.10 payload byte store / 9.6 capture policy):
    input_ref     text,
    output_ref    text,
    output_size   bigint,
    payload_hash  text,
    started_at    timestamptz NOT NULL DEFAULT now(),
    ended_at      timestamptz,
    CONSTRAINT node_runs_frame_check CHECK (frame_id >= 0),
    CONSTRAINT node_runs_frame_relation_check CHECK (
        (frame_id = 0 AND parent_frame_id IS NULL AND call_site_id IS NULL)
        OR
        (frame_id > 0 AND parent_frame_id IS NOT NULL AND parent_frame_id >= 0
         AND parent_frame_id < frame_id AND call_site_id IS NOT NULL
         AND call_site_id ~ '^[a-z0-9-]+$')
    ),
    CONSTRAINT node_runs_plan_hash_check
        CHECK (current_plan_hash ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT node_runs_local_node_check CHECK (local_node_id ~ '^[a-z0-9-]+$'),
    PRIMARY KEY (tenant_id, run_id, frame_id, local_node_id, occurrence),
    FOREIGN KEY (tenant_id, run_id) REFERENCES wamn_run.runs (tenant_id, run_id) ON DELETE CASCADE
);
-- Run history reads rows in dispatch order.
CREATE INDEX node_runs_seq ON wamn_run.node_runs (tenant_id, run_id, seq);
ALTER TABLE wamn_run.node_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.node_runs FORCE ROW LEVEL SECURITY;
CREATE POLICY node_runs_tenant ON wamn_run.node_runs
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT ON wamn_run.node_runs TO wamn_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.node_runs
    TO wamn_run_projection_writer;

-- ---------------------------------------------------------------------------
-- Immutable effect-attempt ledger. `node_runs` remains the current occurrence
-- projection; every effectful occurrence has one server-minted identity here.
-- wamn-0h0g.4.9 installs the inaccessible writer primitive. wamn-0h0g.5.4
-- first wires and activates it; until then execution remains hard-refused.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.effect_attempts (
    tenant_id       text NOT NULL,
    attempt_id      uuid NOT NULL DEFAULT gen_random_uuid(),
    run_id          text NOT NULL,
    root_plan_hash  text NOT NULL,
    current_plan_hash text NOT NULL,
    frame_id        bigint NOT NULL DEFAULT 0,
    parent_frame_id bigint,
    call_site_id    text,
    local_node_id   text NOT NULL,
    source_artifact_hash text NOT NULL,
    requirement_name text NOT NULL,
    occurrence      int NOT NULL,
    seq             int NOT NULL,
    generation_fact_kind text NOT NULL,
    connection_name text,
    connection_generation text,
    credential_generation text,
    verified_author_principal text,
    verified_publisher_principal text,
    attempt_started_at timestamptz NOT NULL DEFAULT now(),
    attempt_deadline_at timestamptz NOT NULL,
    attempt_input_ref text NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT effect_attempts_tenant_check CHECK (tenant_id <> ''),
    CONSTRAINT effect_attempts_root_plan_hash_check
        CHECK (root_plan_hash ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT effect_attempts_current_plan_hash_check
        CHECK (current_plan_hash ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT effect_attempts_frame_check CHECK (frame_id >= 0),
    CONSTRAINT effect_attempts_frame_relation_check CHECK (
        (frame_id = 0 AND parent_frame_id IS NULL AND call_site_id IS NULL)
        OR
        (frame_id > 0 AND parent_frame_id IS NOT NULL AND parent_frame_id >= 0
         AND parent_frame_id < frame_id AND call_site_id IS NOT NULL
         AND call_site_id ~ '^[a-z0-9-]+$')
    ),
    CONSTRAINT effect_attempts_local_node_check CHECK (local_node_id ~ '^[a-z0-9-]+$'),
    CONSTRAINT effect_attempts_source_artifact_check
        CHECK (source_artifact_hash ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT effect_attempts_requirement_check CHECK (requirement_name <> ''),
    CONSTRAINT effect_attempts_occurrence_check CHECK (occurrence >= 0),
    CONSTRAINT effect_attempts_seq_check CHECK (seq >= 0),
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
    PRIMARY KEY (tenant_id, attempt_id),
    CONSTRAINT effect_attempts_occurrence_key
        UNIQUE (tenant_id, run_id, frame_id, local_node_id, occurrence),
    CONSTRAINT effect_attempts_dispatch_identity_key
        UNIQUE (tenant_id, attempt_id, attempt_started_at,
                run_id, frame_id, local_node_id, occurrence)
);
-- Deliberately no FK from (tenant_id, run_id) to runs: effect attempts are an
-- audit ledger with an independent retention lifetime. Pruning terminal run
-- history cascades through the mutable node projection but must leave these
-- immutable facts intact.
CREATE INDEX effect_attempts_bulk_scope
    ON wamn_run.effect_attempts
       (tenant_id, connection_name, connection_generation, attempt_started_at);
ALTER TABLE wamn_run.effect_attempts ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.effect_attempts FORCE ROW LEVEL SECURITY;
CREATE POLICY effect_attempts_tenant ON wamn_run.effect_attempts
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
REVOKE ALL PRIVILEGES ON TABLE wamn_run.effect_attempts
    FROM PUBLIC, wamn_app, wamn_scenario_author, wamn_effect_writer;
GRANT SELECT ON wamn_run.effect_attempts TO wamn_app;
GRANT SELECT, INSERT ON wamn_run.effect_attempts TO wamn_effect_writer;
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
    run_id          text NOT NULL,
    frame_id        bigint NOT NULL,
    local_node_id   text NOT NULL,
    occurrence      int NOT NULL,
    dispatched_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT effect_attempt_dispatches_tenant_check CHECK (tenant_id <> ''),
    CONSTRAINT effect_attempt_dispatches_frame_check CHECK (frame_id >= 0),
    CONSTRAINT effect_attempt_dispatches_local_node_check
        CHECK (local_node_id ~ '^[a-z0-9-]+$'),
    CONSTRAINT effect_attempt_dispatches_occurrence_check CHECK (occurrence >= 0),
    CONSTRAINT effect_attempt_dispatches_time_check
        CHECK (attempt_started_at <= dispatched_at),
    PRIMARY KEY (tenant_id, attempt_id),
    UNIQUE (tenant_id, attempt_id, dispatched_at),
    CONSTRAINT effect_attempt_dispatches_occurrence_key
        UNIQUE (tenant_id, run_id, frame_id, local_node_id, occurrence),
    CONSTRAINT effect_attempt_dispatches_attempt_fk
        FOREIGN KEY (tenant_id, attempt_id, attempt_started_at,
                     run_id, frame_id, local_node_id, occurrence)
        REFERENCES wamn_run.effect_attempts
            (tenant_id, attempt_id, attempt_started_at,
             run_id, frame_id, local_node_id, occurrence)
);
ALTER TABLE wamn_run.effect_attempt_dispatches ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.effect_attempt_dispatches FORCE ROW LEVEL SECURITY;
CREATE POLICY effect_attempt_dispatches_tenant ON wamn_run.effect_attempt_dispatches
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
REVOKE ALL PRIVILEGES ON TABLE wamn_run.effect_attempt_dispatches
    FROM PUBLIC, wamn_app, wamn_scenario_author, wamn_effect_writer;
GRANT SELECT ON wamn_run.effect_attempt_dispatches TO wamn_app;
GRANT SELECT, INSERT ON wamn_run.effect_attempt_dispatches TO wamn_effect_writer;
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
REVOKE ALL PRIVILEGES ON TABLE wamn_run.effect_attempt_outcomes
    FROM PUBLIC, wamn_app, wamn_scenario_author, wamn_effect_writer;
GRANT SELECT ON wamn_run.effect_attempt_outcomes TO wamn_app;
GRANT SELECT, INSERT ON wamn_run.effect_attempt_outcomes TO wamn_effect_writer;
CREATE TRIGGER effect_attempt_outcomes_update_immutable
BEFORE UPDATE ON wamn_run.effect_attempt_outcomes
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_effect_fact_change();
CREATE TRIGGER effect_attempt_outcomes_delete_immutable
BEFORE DELETE ON wamn_run.effect_attempt_outcomes
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_effect_fact_change();

-- ---------------------------------------------------------------------------
-- Immutable operator resolution evidence. There is deliberately no FK to run
-- or node history: terminal history can be pruned without erasing the action.
-- The project-admin transaction supplies SESSION_USER as database-role
-- attribution and appends before changing the mutable terminal projections.
-- ---------------------------------------------------------------------------
CREATE TABLE wamn_run.operator_run_actions (
    tenant_id       text NOT NULL,
    action_id       uuid NOT NULL DEFAULT gen_random_uuid(),
    correlation_id  text NOT NULL,
    run_id          text NOT NULL,
    action_kind     text NOT NULL,
    basis           text NOT NULL,
    evidence_ref    text NOT NULL,
    principal       text NOT NULL,
    principal_kind  text NOT NULL,
    prior_run_status text NOT NULL,
    prior_started_node_frame_id bigint,
    prior_started_node_local_node_id text,
    prior_started_node_occurrence int,
    prior_started_node_status text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT operator_run_actions_tenant_check CHECK (tenant_id <> ''),
    CONSTRAINT operator_run_actions_correlation_check CHECK (correlation_id <> ''),
    CONSTRAINT operator_run_actions_run_check CHECK (run_id <> ''),
    CONSTRAINT operator_run_actions_kind_check
        CHECK (action_kind = 'terminalize-effect-uncertain'),
    CONSTRAINT operator_run_actions_basis_check
        CHECK (basis IN ('external-evidence', 'counterparty-confirmation',
                         'operator-judgment')),
    CONSTRAINT operator_run_actions_evidence_check CHECK (evidence_ref <> ''),
    CONSTRAINT operator_run_actions_principal_check CHECK (principal <> ''),
    CONSTRAINT operator_run_actions_principal_kind_check
        CHECK (principal_kind = 'database-role'),
    CONSTRAINT operator_run_actions_prior_run_status_check
        CHECK (prior_run_status = 'effect-uncertain'),
    CONSTRAINT operator_run_actions_prior_node_check CHECK (
        (prior_started_node_frame_id IS NULL
         AND prior_started_node_local_node_id IS NULL
         AND prior_started_node_occurrence IS NULL
         AND prior_started_node_status IS NULL)
        OR
        (prior_started_node_frame_id >= 0
         AND prior_started_node_local_node_id IS NOT NULL
         AND prior_started_node_local_node_id ~ '^[a-z0-9-]+$'
         AND prior_started_node_occurrence >= 0
         AND prior_started_node_status = 'started')
    ),
    PRIMARY KEY (tenant_id, action_id),
    CONSTRAINT operator_run_actions_run_key UNIQUE (tenant_id, run_id),
    CONSTRAINT operator_run_actions_correlation_key UNIQUE (tenant_id, correlation_id)
);
ALTER TABLE wamn_run.operator_run_actions ENABLE ROW LEVEL SECURITY;
ALTER TABLE wamn_run.operator_run_actions FORCE ROW LEVEL SECURITY;
CREATE POLICY operator_run_actions_tenant ON wamn_run.operator_run_actions
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
REVOKE ALL PRIVILEGES ON TABLE wamn_run.operator_run_actions
    FROM PUBLIC, wamn_app, wamn_scenario_author, wamn_effect_writer;
CREATE TRIGGER operator_run_actions_update_immutable
BEFORE UPDATE ON wamn_run.operator_run_actions
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_operator_run_action_change();
CREATE TRIGGER operator_run_actions_delete_immutable
BEFORE DELETE ON wamn_run.operator_run_actions
FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_immutable_operator_run_action_change();
