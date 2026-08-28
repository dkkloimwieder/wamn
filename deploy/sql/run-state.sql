-- Run-state storage schema (5.7). The production `runs` table persists one row
-- per execution. This is the durable, queryable record behind run history and
-- at-least-once execution. The router is a single-shot in-memory walk; these
-- tables are the facts its host-owned production driver writes.
--
-- STANDALONE ARTIFACT: deliberately NOT included by deploy/sql/postgres-init.sql, the
-- same convention as deploy/sql/catalog-schema.sql (3.1/3.4/3.5/3.6). The S3/S6 gate
-- fixtures carry their own `runs` copy (postgres-init.sql schema
-- `s3`) so flowbench exercises the rewired runner; this file is the production schema and the
-- target of the crate's live-apply gate. Assumes pre-existing `wamn_app`,
-- `wamn_scenario_author` and stable `wamn_effect_writer` ACL roles;
-- `wamn_platform` and `wamn_run_retention` are the two roles this file creates
-- itself, because it NAMES them -- the floor arms below target the first
-- (`wamn-0h0g.22.17`) and the `runs` grants target the second
-- (`wamn-0h0g.12.69`) -- and naming a role that may not exist is an apply-time
-- failure rather than a degradation. Role and
-- scoped LOGIN credential-generation lifecycle is provisioning-owned; this
-- artifact grants the stable role ledger append/read authority plus only the
-- narrow run columns needed for its fenced runnable-state recheck.
--
-- Security shape mirrors the rest of the platform (s2/s3, catalog): tenant
-- separation from `current_user` (wamn-0h0g.22.6). Every guest-reachable table
-- FORCEs RLS keyed on
--   wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()
-- and carries the matching `<table>_tkey` expression index; CHECK
-- (tenant_id <> '') forbids a ''-tenant row, so the floor is structural.
--
-- `operator_run_actions` deliberately KEEPS its `app.tenant` predicate: the
-- guest ACL role holds no privilege on it (measured from has_table_privilege,
-- wamn-0h0g.22.6.3), so its claim is HOST-INJECTED and re-keying it would be
-- change without a threat. `wamn_run.run_queue` is the same case in
-- deploy/sql/run-queue.sql.
--
-- SCOPE (what 5.7 does NOT own, reserved as nullable seams below): the durable
-- run QUEUE + leases + doorbell (5.14) co-transact with these INSERTs but own
-- their own table; the run-level I/O CAPTURE policy (9.6) is fixed at admission
-- and full capture fills scrubbed `input_json`/`output_json` plus the output
-- size and optional hash; the content-addressed payload BYTE store (5.10) is
-- pointed at by `input_ref`/`output_ref`.

CREATE SCHEMA IF NOT EXISTS wamn_run AUTHORIZATION CURRENT_USER;

-- The shared platform group role every non-guest tenant-floor arm below targets
-- (`wamn-0h0g.22.17`). Created HERE, not assumed, for the reason
-- `crates/control/provision/src/tenant_key.rs` already gives for its own
-- bootstrap: naming a role that may not exist adds a new precondition to every
-- applier — seven live gates plus the production path in `services/ctl` — and
-- `CREATE POLICY ... TO wamn_platform` fails outright against a cluster without
-- it.
--
-- EXCEPTION-guarded under the shared `wamn_role_bootstrap` advisory lock, the
-- `ensure_platform_group_role_sql` shape. Roles are CLUSTER-global, so two
-- appliers that do not both take the lock can each observe the role absent and
-- both issue `CREATE ROLE`.
DO $platform_group$ BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles
                 WHERE rolname = 'wamn_platform') THEN
    CREATE ROLE wamn_platform NOLOGIN NOSUPERUSER NOCREATEDB
      NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
  END IF;
EXCEPTION WHEN duplicate_object THEN NULL;
END $platform_group$;

-- The stable run-retention ACL role the `runs` grants below name
-- (`wamn-0h0g.12.69`). Created HERE for exactly the reason `wamn_platform` is:
-- a `GRANT ... TO` a role that does not exist fails the whole apply, and every
-- in-tree applier of this artifact -- a dozen live gates plus the production
-- reconcile path -- would otherwise acquire a new precondition. The stable role
-- is a grant carrier only; its scoped A/B LOGIN generations, their CONNECT and
-- their Secret are provisioning-owned, exactly as the effect writer's are.
--
-- EXCEPTION-guarded under the shared advisory lock, the same shape and for the
-- same reason: roles are CLUSTER-global and two appliers can each observe the
-- role absent.
DO $run_retention$ BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles
                 WHERE rolname = 'wamn_run_retention') THEN
    CREATE ROLE wamn_run_retention NOLOGIN NOSUPERUSER NOCREATEDB
      NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
  END IF;
EXCEPTION WHEN duplicate_object THEN NULL;
END $run_retention$;
REVOKE ALL PRIVILEGES ON SCHEMA wamn_run
    FROM PUBLIC, wamn_effect_writer, wamn_run_retention;
GRANT USAGE ON SCHEMA wamn_run TO wamn_app;
GRANT USAGE ON SCHEMA wamn_run TO wamn_scenario_author;
GRANT USAGE ON SCHEMA wamn_run TO wamn_effect_writer;
GRANT USAGE ON SCHEMA wamn_run TO wamn_run_retention;

-- ---------------------------------------------------------------------------
-- The per-database authority derivations every tenant policy below calls
-- (`wamn-0h0g.22.6`). Guest tenant authority comes from `current_user`, not
-- from a claim the session can set: a session that can set its own tenant can
-- read every tenant.
--
-- GENERATED — this block is `authority_derivations_bootstrap_sql()` verbatim,
-- pinned by a byte-equality guard in wamn-control-provision. Do not hand-edit;
-- change the builder.
--
-- It is the BOOTSTRAP rendering because this file is applied to databases whose
-- names are not known here, and `tenant_key` must bake the name in as a literal
-- to stay `IMMUTABLE` — without which the expression indexes below are not even
-- creatable.
-- ---------------------------------------------------------------------------
DO $wamn_authority_bootstrap$
BEGIN
    EXECUTE replace(replace($wamn_authority_derivations$CREATE SCHEMA IF NOT EXISTS "wamn_authority" AUTHORIZATION CURRENT_USER;
REVOKE ALL ON SCHEMA "wamn_authority" FROM PUBLIC;
GRANT USAGE ON SCHEMA "wamn_authority" TO "wamn_app";
CREATE OR REPLACE FUNCTION "wamn_authority".tenant_key(tenant text)
RETURNS text
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
STRICT
SET search_path = pg_catalog
AS $$
    SELECT substr(encode(sha256(
           int8send(19::bigint) || convert_to('wamn.app.scope.v0.1', 'UTF8')
        || int8send(6::bigint) || convert_to('tenant', 'UTF8')
        || int8send(octet_length(convert_to(tenant, 'UTF8'))::bigint) || convert_to(tenant, 'UTF8')
        || int8send(8::bigint) || convert_to('database', 'UTF8')
        || int8send(@wamn_database_octets@::bigint) || convert_to(@wamn_database_literal@, 'UTF8')
       ), 'hex'), 1, 40)
$$;
ALTER FUNCTION "wamn_authority".tenant_key(text) OWNER TO CURRENT_USER;
REVOKE ALL ON FUNCTION "wamn_authority".tenant_key(text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION "wamn_authority".tenant_key(text) TO "wamn_app";
CREATE OR REPLACE FUNCTION "wamn_authority".current_tenant_key()
RETURNS text
LANGUAGE sql
STABLE
PARALLEL SAFE
SET search_path = pg_catalog
AS $$
    SELECT substring(current_user::text from '^wamn_app_([0-9a-f]{40})_[ab]$')
$$;
ALTER FUNCTION "wamn_authority".current_tenant_key() OWNER TO CURRENT_USER;
REVOKE ALL ON FUNCTION "wamn_authority".current_tenant_key() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION "wamn_authority".current_tenant_key() TO "wamn_app";$wamn_authority_derivations$,
        '@wamn_database_literal@', quote_literal(current_database())),
        '@wamn_database_octets@', octet_length(convert_to(current_database(), 'UTF8'))::text);
END
$wamn_authority_bootstrap$;

-- Closed run-queue operation classes are database facts derived from the
-- authenticated current_user. These assertion functions are ordinary
-- SECURITY INVOKER functions: they confer no authority, and their only effect
-- is the stable typed refusal embedded by each role-specific statement.
CREATE FUNCTION wamn_run.require_executor_platform_authority()
RETURNS boolean
LANGUAGE plpgsql
SECURITY INVOKER
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pg_catalog.pg_roles AS authority
         WHERE authority.rolname = 'wamn_executor_platform'
           AND pg_catalog.pg_has_role(CURRENT_USER, authority.oid, 'MEMBER')
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = '42501',
            MESSAGE = 'executor-platform-authority-required';
    END IF;
    RETURN true;
END
$$;

CREATE FUNCTION wamn_run.require_management_admission_authority()
RETURNS boolean
LANGUAGE plpgsql
SECURITY INVOKER
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pg_catalog.pg_roles AS authority
         WHERE authority.rolname = 'wamn_management_admitter'
           AND pg_catalog.pg_has_role(CURRENT_USER, authority.oid, 'MEMBER')
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = '42501',
            MESSAGE = 'management-admission-authority-required';
    END IF;
    RETURN true;
END
$$;

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
--   * STILL RUNNABLE. A terminal run keeps the audit link to the release closure
--     it finished under. Nothing reopens a terminal run's claimability, so nothing
--     needs to erase it.
--   * NO IMMUTABLE EFFECT ATTEMPT, ON A `durable` RUN. An attributed effect
--     names the release that fired it, and that link is never rewritten out
--     from under it. This is the leg that refuses a mid-effect release rewrite;
--     a run carrying an attempt is classified terminal effect-uncertain by its
--     next claim and never re-executes under a second release.
--     THE CLASS PREDICATE IS LOAD-BEARING, NOT DECORATION (wamn-0h0g.20.2).
--     Only the executor's pre-effect reclaim can clear this pair, and it first
--     classifies the immutable attempt ledger under the durable-class rule.
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
    IF NEW.flow_id IS DISTINCT FROM OLD.flow_id
       OR NEW.flow_version IS DISTINCT FROM OLD.flow_version
       OR NEW.catalog_id IS DISTINCT FROM OLD.catalog_id
       OR NEW.catalog_version IS DISTINCT FROM OLD.catalog_version
       OR NEW.environment IS DISTINCT FROM OLD.environment
       OR NEW.capture_mode IS DISTINCT FROM OLD.capture_mode
       OR NEW.durability_class IS DISTINCT FROM OLD.durability_class
       OR NEW.wiring_id IS DISTINCT FROM OLD.wiring_id
       OR NEW.wiring_version IS DISTINCT FROM OLD.wiring_version
       OR NEW.wiring_hash IS DISTINCT FROM OLD.wiring_hash
       OR NEW.binding_world_json IS DISTINCT FROM OLD.binding_world_json THEN
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

-- `wamn_run_retention` holds DELETE for history pruning, and `wamn_app` still
-- retains it pending the guest-login retirement. The table grant cannot express
-- the prune statement's terminal-state predicate, so this ordinary
-- invoker-rights trigger makes that predicate caller-independent. An
-- `effect-uncertain` run is deliberately not terminal: its operator resolution
-- remains part of the durable audit floor and cannot be pruned as history
-- (wamn-0h0g.12.128).
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
TO wamn_app
USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY environment_policies_platform ON wamn_run.environment_policies
    AS PERMISSIVE FOR SELECT TO wamn_platform
    USING (true);
CREATE INDEX environment_policies_tkey
    ON wamn_run.environment_policies ((wamn_authority.tenant_key(tenant_id)));
REVOKE ALL PRIVILEGES ON TABLE wamn_run.environment_policies
    FROM PUBLIC, wamn_app, wamn_scenario_author, wamn_effect_writer;
GRANT SELECT ON TABLE wamn_run.environment_policies TO wamn_app;

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
    -- Nullable legacy execution grain. New admissions leave both NULL and
    -- carry the complete component-era wiring grain below; no migration
    -- fabricates component provenance for historical flow rows.
    flow_id         text,
    flow_version    int,
    catalog_id      text NOT NULL,
    catalog_version int NOT NULL,
    environment     text NOT NULL,
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
    -- Immutable execution identity selected from the trusted active-wiring
    -- pointer by the admission transaction. Legacy rows remain NULL until the
    -- post-drain cutover; admission never backfills them.
    --
    -- `wiring_hash` is the WHOLE candidate identity (wamn-0h0g.8.5.6): the gate
    -- report that certified the definition is keyed by that same hash, so the
    -- `gate_report_id` that used to sit beside it here carried no fact this
    -- column does not already carry.
    wiring_id       text,
    wiring_version  int,
    wiring_hash     text,
    -- Canonically ordered, non-secret requirement/binding/generation facts
    -- derived by private management admission. Array order is
    -- (component-digest, store-alias); callers never author this value.
    binding_world_json jsonb,
    -- The claim-time release record. A run is NOT version-pinned at admission:
    -- it executes under the release its CLAIMING pod carries, and the worker
    -- writes that pod's own release identity here when it takes the lease,
    -- once per claim attempt, enforced by `guard_run_admission_pins_immutable`.
    -- Both are NULL on the admitted row and move together. `release_version`
    -- is the release (catalog) version; `manifest_digest` is the RFC 8785
    -- digest of that release's component/interface/wiring serving closure.
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
           AND environment <> ''),
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
    CONSTRAINT runs_wiring_identity_check CHECK (
      (wiring_id IS NULL AND wiring_version IS NULL)
      OR (wiring_id IS NOT NULL AND wiring_version IS NOT NULL
          AND wiring_id <> '' AND wiring_version > 0)
    ),
    -- Exactly one complete historical or component-era execution grain. The
    -- explicit IS NOT NULL arms keep PostgreSQL CHECK's NULL truth value from
    -- admitting a half-record. Existing flow rows stay truthful; they are not
    -- backfilled with an identity that was never recorded.
    CONSTRAINT runs_execution_grain_check CHECK (
      (flow_id IS NOT NULL AND flow_version IS NOT NULL
       AND flow_id <> '' AND flow_version > 0
       AND wiring_hash IS NULL
       AND binding_world_json IS NULL)
      OR
      (flow_id IS NULL AND flow_version IS NULL
       AND wiring_id IS NOT NULL AND wiring_version IS NOT NULL
       AND wiring_id <> '' AND wiring_version > 0
       AND wiring_hash IS NOT NULL
       AND wiring_hash ~ '^sha256:[0-9a-f]{64}$'
       AND binding_world_json IS NOT NULL
       AND jsonb_typeof(binding_world_json) = 'array')
    ),
    PRIMARY KEY (tenant_id, run_id),
    CONSTRAINT runs_release_fk
        FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.releases (tenant_id, catalog_id, catalog_version)
);
-- At-least-once: a redelivered trigger with the same key collapses to one run.
CREATE UNIQUE INDEX runs_idempotency ON wamn_run.runs (tenant_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
-- History listing and trusted CDC event-causation traversal.
CREATE INDEX runs_flow ON wamn_run.runs (tenant_id, flow_id, created_at);
CREATE INDEX runs_release ON wamn_run.runs (tenant_id, catalog_id, catalog_version);
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
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY runs_platform ON wamn_run.runs
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
-- THE EFFECT-WRITER ARM, AND WHY IT IS `USING (true)` (`wamn-0h0g.22.32`).
--
-- `wamn_effect_writer` is not a `wamn_platform` member and cannot become one:
-- the stable role's own shape guard refuses ANY row in `pg_auth_members` for
-- it, so the shared group edge and that guard cannot both hold. Without an arm
-- of its own the writer matches no policy at all, and PostgreSQL DEFAULT-DENIES
-- at zero rows, in silence, under FORCE RLS. This is that arm.
--
-- The arm is unqualified rather than tenant-scoped because ONE PROJECT-
-- ENVIRONMENT DATABASE SERVES EXACTLY ONE TENANT. A row-level tenant predicate
-- here would be a wall against a neighbour that structurally cannot exist in
-- this plane; the walls that carry the isolation are the database boundary and
-- the credential. What still bounds the writer is its TABLE GRANT, which on
-- this relation is `SELECT (tenant_id, run_id, status)` and nothing else.
--
-- A tenant-scoped predicate is also not EXPRESSIBLE here, which is a separate
-- fact from the one above. `wamn_authority.current_tenant_key()` recovers a key
-- only from the `wamn_app_<40hex>_[ab]` guest generation pattern and derives
-- NULL for every effect-writer login; widening that regex would not help,
-- because the guest digest is taken over scope domain `wamn.app.scope.v0.1`
-- while the effect-writer login digest is over `wamn.effect-writer.scope.v0.1`,
-- so the two values can never compare equal. Re-deriving the writer over the
-- guest domain is refused.
--
-- If a tier ever places two tenants in one database, this arm is where that is
-- re-decided. The complexity defers to that tier and is deliberately not
-- carried here.
CREATE POLICY runs_effect_writer ON wamn_run.runs
    AS PERMISSIVE FOR ALL TO wamn_effect_writer
    USING (true)
    WITH CHECK (true);
CREATE INDEX runs_tkey
    ON wamn_run.runs ((wamn_authority.tenant_key(tenant_id)));

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
BEFORE UPDATE OF flow_id, flow_version, catalog_id, catalog_version, environment,
                 capture_mode, durability_class, wiring_id, wiring_version,
                 wiring_hash, binding_world_json,
                 release_version, manifest_digest
ON wamn_run.runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_run_admission_pins_immutable();
CREATE TRIGGER runs_terminal_delete_only
BEFORE DELETE ON wamn_run.runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_terminal_run_delete();
-- Hot HTTP and stream delivery no longer author runs, and executor claim/reap
-- no longer belongs to the guest ACL. `wamn_app` therefore retains only the
-- read and terminal-history pruning surface; private management INSERT and
-- executor UPDATE grants are provisioned to their dedicated authorities by
-- their owning cutovers. `runs_terminal_delete_only` still confines retention.
--
-- `wamn_run_retention` (`wamn-0h0g.12.69`) is run history pruning's OWN
-- principal, and the grant below is its ENTIRE authority anywhere in the
-- cluster: `DELETE`, plus `SELECT` on EXACTLY the three columns the prune
-- statement's WHERE clause reads. The `SELECT` is COLUMN-SCOPED and that is
-- load bearing, not tidiness. `wamn_run_retention` is a member of
-- `wamn_platform`, whose one permissive floor arm is `USING (true)`, so a
-- table-level `SELECT` here would let a retention credential read EVERY
-- tenant's `input_json`, `result_json` and `state_json` — measured. Column
-- scoping is the only grant-shaped bound available: PostgreSQL privileges are
-- relation- and column-shaped, never row-shaped, so what the shared arm buys
-- retention is limited HERE or nowhere.
--
-- The membership itself is not optional. Measured on PostgreSQL 18.6: with the
-- `wamn_platform` edge revoked, a retention generation holding exactly these
-- grants reads zero rows and deletes zero rows — RLS is FORCEd and no policy
-- matches the connected role, so PostgreSQL DEFAULT-DENIES silently. The arm is
-- what makes retention work at all; the column list is what keeps it cheap.
--
-- It needs nothing on `wamn_run.run_queue`: measured, the `ON DELETE CASCADE`
-- referential action in deploy/sql/run-queue.sql fires as an internal
-- referential-integrity trigger that consults neither the deleter's table grants
-- nor that relation's FORCE RLS policy, so a retention session holding zero
-- queue privilege still cascades the queue row away. That is also the ONLY
-- cascade out of `runs`: every other foreign key in the run plane is
-- `NO ACTION`, so the effect ledgers are not reachable from a run delete and
-- retention is granted nothing on them.
--
-- THE ACCEPTED RESIDUAL (`wamn-0h0g.22.34`). Column scoping bounds WHAT a
-- retention credential may read; it bounds nothing about WHOSE rows it may
-- delete. A raw session holding a retention generation credential can DELETE
-- another tenant's terminal runs, because the shared `USING (true)` floor arm
-- matches every row and PostgreSQL privileges are relation- and column-shaped,
-- never row-shaped. There is no grant that expresses "only this tenant's rows".
--
-- This is ACCEPTED, not overlooked, on the same silo reasoning the effect-writer
-- arm above records: ONE PROJECT-ENVIRONMENT DATABASE SERVES EXACTLY ONE
-- TENANT, so the "other tenant" the residual reaches does not exist in this
-- plane. The walls are the database boundary and the credential.
--
-- `runs_terminal_delete_only` REMAINS THE GUARD and must not be weakened: it is
-- what keeps the residual confined to terminal history rather than live runs,
-- and it is caller-independent because it is an invoker-rights trigger rather
-- than a grant. If a tier ever places two tenants in one database, the shape to
-- reach for is a tenant arm on that trigger. It is deliberately NOT built here,
-- because today it would guard against a neighbour that cannot exist.
REVOKE ALL PRIVILEGES ON TABLE wamn_run.runs
    FROM PUBLIC, wamn_app, wamn_effect_writer, wamn_run_retention;
GRANT SELECT, DELETE ON wamn_run.runs TO wamn_app;
GRANT SELECT (tenant_id, status, created_at), DELETE
    ON wamn_run.runs TO wamn_run_retention;
-- The private effect writer may only recheck that the fenced run still has
-- runnable state. Lease-generation authority remains outside this schema lane.
GRANT SELECT (tenant_id, run_id, status)
    ON wamn_run.runs TO wamn_effect_writer;

-- ---------------------------------------------------------------------------
-- Immutable effect-attempt ledger. Every effectful occurrence has one
-- server-minted identity here.
-- wamn-0h0g.4.9 installs the inaccessible writer primitive. Whoever first
-- wires and activates it lifts the refusal; until then execution remains
-- hard-refused.
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
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY effect_attempts_platform ON wamn_run.effect_attempts
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
-- The effect-writer arm, on the same footing as `runs_effect_writer` above and
-- for the same reason (`wamn-0h0g.22.32`): `wamn_effect_writer` cannot hold the
-- `wamn_platform` membership its own shape guard forbids, so without an arm
-- naming it this ledger default-denies to zero rows in silence. It is
-- `USING (true)` rather than tenant-scoped because ONE PROJECT-ENVIRONMENT
-- DATABASE SERVES EXACTLY ONE TENANT — the row-level tenant boundary it would
-- express is a wall against a neighbour that cannot exist in this plane, and
-- `current_tenant_key` derives NULL for an effect-writer login in any case. The
-- writer's table grant below (`SELECT`, and no append) is what bounds it here.
CREATE POLICY effect_attempts_effect_writer ON wamn_run.effect_attempts
    AS PERMISSIVE FOR ALL TO wamn_effect_writer
    USING (true)
    WITH CHECK (true);
CREATE INDEX effect_attempts_tkey
    ON wamn_run.effect_attempts ((wamn_authority.tenant_key(tenant_id)));
REVOKE ALL PRIVILEGES ON TABLE wamn_run.effect_attempts
    FROM PUBLIC, wamn_app, wamn_scenario_author, wamn_effect_writer;
GRANT SELECT ON wamn_run.effect_attempts TO wamn_app;
-- BORN PARKED. The writer's APPEND authority is deliberately absent below: the
-- primitive is installed but unwired (wamn-0h0g.4.9), and every provisioned
-- generation login inherits `wamn_effect_writer` with INHERIT TRUE, so a live
-- INSERT here would be dormant authority mass-produced by the provisioner. A
-- fresh environment is therefore refused by the SERVER (42501), not by prose.
-- Whoever wires the writer grants this INSERT explicitly and re-proves this
-- gate; the record deliberately no longer arms it for them.
GRANT SELECT ON wamn_run.effect_attempts TO wamn_effect_writer;
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
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY effect_attempt_dispatches_platform ON wamn_run.effect_attempt_dispatches
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
-- The effect-writer arm, for the reason `runs_effect_writer` carries in full
-- (`wamn-0h0g.22.32`): the writer's stable role may hold no membership, so
-- without an arm naming it this ledger default-denies to zero rows in silence.
-- `USING (true)` rather than tenant-scoped because ONE PROJECT-ENVIRONMENT
-- DATABASE SERVES EXACTLY ONE TENANT — the tenant boundary it would express is
-- a wall against a neighbour that cannot exist in this plane. The writer's
-- table grant below is what bounds it here.
CREATE POLICY effect_attempt_dispatches_effect_writer ON wamn_run.effect_attempt_dispatches
    AS PERMISSIVE FOR ALL TO wamn_effect_writer
    USING (true)
    WITH CHECK (true);
CREATE INDEX effect_attempt_dispatches_tkey
    ON wamn_run.effect_attempt_dispatches ((wamn_authority.tenant_key(tenant_id)));
REVOKE ALL PRIVILEGES ON TABLE wamn_run.effect_attempt_dispatches
    FROM PUBLIC, wamn_app, wamn_scenario_author, wamn_effect_writer;
GRANT SELECT ON wamn_run.effect_attempt_dispatches TO wamn_app;
-- BORN PARKED, for the reason the attempt ledger above is: the writer primitive
-- is installed but unwired, and every provisioned generation login inherits
-- `wamn_effect_writer` with INHERIT TRUE, so a live INSERT here would be dormant
-- authority mass-produced by the provisioner. A fresh environment is refused by
-- the SERVER (42501), not by prose. Whoever wires the writer grants this INSERT
-- explicitly and re-proves the gate; the record deliberately does not arm it.
GRANT SELECT ON wamn_run.effect_attempt_dispatches TO wamn_effect_writer;
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
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY effect_attempt_outcomes_platform ON wamn_run.effect_attempt_outcomes
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
-- The effect-writer arm, for the reason `runs_effect_writer` carries in full
-- (`wamn-0h0g.22.32`): the writer's stable role may hold no membership, so
-- without an arm naming it this ledger default-denies to zero rows in silence.
-- `USING (true)` rather than tenant-scoped because ONE PROJECT-ENVIRONMENT
-- DATABASE SERVES EXACTLY ONE TENANT — the tenant boundary it would express is
-- a wall against a neighbour that cannot exist in this plane. The writer's
-- table grant below is what bounds it here.
CREATE POLICY effect_attempt_outcomes_effect_writer ON wamn_run.effect_attempt_outcomes
    AS PERMISSIVE FOR ALL TO wamn_effect_writer
    USING (true)
    WITH CHECK (true);
CREATE INDEX effect_attempt_outcomes_tkey
    ON wamn_run.effect_attempt_outcomes ((wamn_authority.tenant_key(tenant_id)));
REVOKE ALL PRIVILEGES ON TABLE wamn_run.effect_attempt_outcomes
    FROM PUBLIC, wamn_app, wamn_scenario_author, wamn_effect_writer;
GRANT SELECT ON wamn_run.effect_attempt_outcomes TO wamn_app;
-- BORN PARKED, on the same footing as the two ledgers above: the primitive is
-- installed but unwired, the stable role is inherited by every provisioned
-- generation LOGIN, so append here would be dormant authority. The SERVER
-- refuses a fresh environment (42501). Whoever wires the writer grants this
-- INSERT explicitly and re-proves the gate; the record does not arm it.
GRANT SELECT ON wamn_run.effect_attempt_outcomes TO wamn_effect_writer;
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
