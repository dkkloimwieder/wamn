-- S2 fixture: schema, RLS, seed data for the wamn:postgres plugin PoC.
-- Runs once at database init (docker-entrypoint-initdb.d locally, ConfigMap
-- mount in the kind cluster), as the postgres superuser.
--
-- Security shape under test: ONE stable ACL role (wamn_app, not owner, no
-- BYPASSRLS) and tenant separation from `current_user` (wamn-0h0g.22.6). RLS
-- policies key on
--   wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()
-- so a session that can set its own claim gains nothing: the connected role is
-- the one thing it cannot rewrite. A role outside the guest generation
-- convention derives NULL and matches no row, and CHECK (tenant_id <> '')
-- forbids a ''-tenant row, so the floor is structural rather than conventional.
--
-- `wamn-0h0g.22.17` gives every governed relation TWO arms: the tenant floor,
-- narrowed `TO wamn_app`, and one permissive `TO wamn_platform` arm for the
-- platform-grain principals whose login names carry no tenant at all.

CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS;

-- The host-only scenario-author group role (11.2/12g). Roles are CLUSTER-global,
-- so creating it here is what makes the canonical run-plane DDL
-- (deploy/sql/{run-state,authoring-tests,catalog-schema}.sql, which GRANT to it)
-- appliable out of the box — without it every such apply dies with
-- `role "wamn_scenario_author" does not exist`. Same advisory-locked,
-- idempotent shape as `wamn_schema_control::ensure_scenario_author_role_sql`
-- (crates/schema/control/src/run_plane.rs), so an init-time create and a later
-- reconcile-run-plane bootstrap converge on identical attributes.
DO $scenario_author$ BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles
                 WHERE rolname = 'wamn_scenario_author') THEN
    CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB
      NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
  END IF;
END $scenario_author$;

-- Stable host-only ACL role for the effect ledger. Credential generations are
-- provisioned separately; this local fixture needs only the NOLOGIN grant
-- carrier so canonical run-state.sql can be applied.
DO $effect_writer$ BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles
                 WHERE rolname = 'wamn_effect_writer') THEN
    CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB
      NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
  END IF;
END $effect_writer$;

-- The shared platform group role every non-guest floor arm targets
-- (`wamn-0h0g.22.17`). PostgreSQL DEFAULT-DENIES when RLS is enabled and no
-- policy matches the connected role, so narrowing the floor `TO wamn_app` does
-- not exempt a platform principal — it locks it out, and silently, at zero
-- rows. The one permissive arm per relation is `TO wamn_platform`; this is the
-- role it names. NOLOGIN, no grants of its own, NOBYPASSRLS: it carries policy
-- membership only.
--
-- EXCEPTION-guarded under the same advisory lock, not bare `IF NOT EXISTS`:
-- roles are cluster-global, so two appliers that do not both take the lock can
-- each observe the role absent and both issue `CREATE ROLE`.
DO $platform_group$ BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles
                 WHERE rolname = 'wamn_platform') THEN
    CREATE ROLE wamn_platform NOLOGIN NOSUPERUSER NOCREATEDB
      NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
  END IF;
EXCEPTION WHEN duplicate_object THEN NULL;
END $platform_group$;

-- INHERIT TRUE IS SPELLED, NOT DEFAULTED, AND THE OMISSION IS SILENT.
-- Both roles below are NOINHERIT, and in PostgreSQL 16+ a role's `rolinherit`
-- supplies the DEFAULT `INHERIT` option for memberships granted TO it — so a
-- bare `GRANT wamn_platform TO wamn_effect_writer` lands `inherit_option =
-- false`, the two-hop chain (generation login -> ACL role -> wamn_platform)
-- dies, and the platform principal reads ZERO ROWS with no error at all.
-- Measured on PostgreSQL 18.6: bare grant -> 0 rows, `INHERIT TRUE` -> all rows.
GRANT wamn_platform TO wamn_scenario_author WITH ADMIN FALSE, INHERIT TRUE, SET FALSE;
GRANT wamn_platform TO wamn_effect_writer WITH ADMIN FALSE, INHERIT TRUE, SET FALSE;

CREATE DATABASE wamn OWNER postgres;

\connect wamn

CREATE SCHEMA s2 AUTHORIZATION postgres;
GRANT USAGE ON SCHEMA s2 TO wamn_app;

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

-- ---------------------------------------------------------------------------
-- Bench target: single-statement query with 8 params returning 10 rows.
-- 20 rows per (tenant, g) group so LIMIT 10 always has headroom.
-- ---------------------------------------------------------------------------
CREATE TABLE s2.bench (
    id      bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id text NOT NULL CHECK (tenant_id <> ''),
    g       int NOT NULL,
    a       int NOT NULL,
    b       bigint NOT NULL,
    c       double precision NOT NULL,
    num     numeric(12,4) NOT NULL,
    ts      timestamptz NOT NULL,
    payload text NOT NULL
);

INSERT INTO s2.bench (tenant_id, g, a, b, c, num, ts, payload)
SELECT t.tenant,
       gs % 1000,
       gs % 100,
       gs::bigint * 1000,
       gs::double precision / 3.0,
       ((gs % 100000)::numeric) / 100,
       TIMESTAMPTZ '2026-01-01 00:00:00+00' + (gs % 86400) * INTERVAL '1 second',
       'payload-' || gs
FROM generate_series(1, 20000) gs,
     (VALUES ('tenant-a'), ('tenant-b')) t(tenant);

CREATE INDEX bench_tenant_g_id ON s2.bench (tenant_id, g, id);

ALTER TABLE s2.bench ENABLE ROW LEVEL SECURITY;
ALTER TABLE s2.bench FORCE ROW LEVEL SECURITY;
CREATE POLICY bench_tenant ON s2.bench
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY bench_platform ON s2.bench
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX bench_tkey
    ON s2.bench ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT, INSERT, UPDATE, DELETE ON s2.bench TO wamn_app;

-- ---------------------------------------------------------------------------
-- RLS gate target: per-tenant secrets that must never cross tenants.
-- ---------------------------------------------------------------------------
CREATE TABLE s2.rls_secrets (
    id        bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id text NOT NULL CHECK (tenant_id <> ''),
    secret    text NOT NULL
);

INSERT INTO s2.rls_secrets (tenant_id, secret)
SELECT t.tenant, 'secret-' || t.tenant || '-' || gs
FROM generate_series(1, 1000) gs,
     (VALUES ('tenant-a'), ('tenant-b')) t(tenant);

ALTER TABLE s2.rls_secrets ENABLE ROW LEVEL SECURITY;
ALTER TABLE s2.rls_secrets FORCE ROW LEVEL SECURITY;
CREATE POLICY rls_secrets_tenant ON s2.rls_secrets
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY rls_secrets_platform ON s2.rls_secrets
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX rls_secrets_tkey
    ON s2.rls_secrets ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT, INSERT, UPDATE, DELETE ON s2.rls_secrets TO wamn_app;

-- ---------------------------------------------------------------------------
-- Scratch table: chaos-gate transactions, injection round-trips, smoke tests.
-- One column per sql-value shape that needs a byte-identical round-trip.
-- ---------------------------------------------------------------------------
CREATE TABLE s2.scratch (
    id        bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id text NOT NULL CHECK (tenant_id <> ''),
    k         text NOT NULL,
    v         text,
    vb        bytea,
    vn        numeric,
    vts       timestamptz,
    vj        jsonb,
    CONSTRAINT scratch_tenant_k_uniq UNIQUE (tenant_id, k),
    CONSTRAINT scratch_k_check CHECK (k <> 'forbidden')
);

ALTER TABLE s2.scratch ENABLE ROW LEVEL SECURITY;
ALTER TABLE s2.scratch FORCE ROW LEVEL SECURITY;
CREATE POLICY scratch_tenant ON s2.scratch
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY scratch_platform ON s2.scratch
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX scratch_tkey
    ON s2.scratch ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT, INSERT, UPDATE, DELETE ON s2.scratch TO wamn_app;

-- FK-violation fixture (FK checks run as table owner and bypass RLS; the
-- guest only needs a way to trip 23503).
CREATE TABLE s2.fkchild (
    id        bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id text NOT NULL CHECK (tenant_id <> ''),
    parent_id bigint NOT NULL CONSTRAINT fkchild_parent_fk REFERENCES s2.scratch (id)
);

ALTER TABLE s2.fkchild ENABLE ROW LEVEL SECURITY;
ALTER TABLE s2.fkchild FORCE ROW LEVEL SECURITY;
CREATE POLICY fkchild_tenant ON s2.fkchild
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY fkchild_platform ON s2.fkchild
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX fkchild_tkey
    ON s2.fkchild ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT, INSERT, UPDATE, DELETE ON s2.fkchild TO wamn_app;

-- Identity columns: inserts by wamn_app need the backing sequences.
GRANT USAGE ON ALL SEQUENCES IN SCHEMA s2 TO wamn_app;

-- ===========================================================================
-- S3 fixture: flow catalog, production-shaped run history, and an idempotent
-- sink for the runner PoC (docs/archive/p0-exit-criteria.md S3). Same security
-- shape as s2: one ACL role, tenant separation from current_user + RLS.
-- The runner reads the catalog and writes run history and the sink entirely
-- through the wamn:postgres capability under its injected claim.
-- ===========================================================================
CREATE SCHEMA s3 AUTHORIZATION postgres;
GRANT USAGE ON SCHEMA s3 TO wamn_app;

-- Flow catalog: the versioned IR (graph_json) with an active-version pointer.
-- "Deploy" = flip `active` to a version. In production this write is a
-- control-plane action and the runner only READS; for the PoC the runner
-- performs the flip (seed / set-active exports) so the whole gate exercises a
-- single wamn:postgres path.
CREATE TABLE s3.flows (
    tenant_id  text NOT NULL CHECK (tenant_id <> ''),
    flow_id    text NOT NULL,
    version    int  NOT NULL,
    active     boolean NOT NULL DEFAULT false,
    graph_json jsonb NOT NULL,
    PRIMARY KEY (tenant_id, flow_id, version)
);
ALTER TABLE s3.flows ENABLE ROW LEVEL SECURITY;
ALTER TABLE s3.flows FORCE ROW LEVEL SECURITY;
CREATE POLICY flows_tenant ON s3.flows
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY flows_platform ON s3.flows
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX flows_tkey
    ON s3.flows ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT, INSERT, UPDATE, DELETE ON s3.flows TO wamn_app;

-- Business side-effect sink. The idempotency key (tenant_id, run_id, step)
-- makes duplicate delivery of the pg-write node's INSERT ... ON CONFLICT DO
-- NOTHING a no-op, so one logical write leaves exactly one row.
CREATE TABLE s3.sink (
    tenant_id  text NOT NULL CHECK (tenant_id <> ''),
    run_id     text NOT NULL,
    step       int  NOT NULL,
    payload    text NOT NULL,
    CONSTRAINT sink_idem UNIQUE (tenant_id, run_id, step)
);
ALTER TABLE s3.sink ENABLE ROW LEVEL SECURITY;
ALTER TABLE s3.sink FORCE ROW LEVEL SECURITY;
CREATE POLICY sink_tenant ON s3.sink
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY sink_platform ON s3.sink
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX sink_tkey
    ON s3.sink ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT, INSERT, UPDATE, DELETE ON s3.sink TO wamn_app;

-- Production-shaped run history (5.7). This s3-schema copy lets flowbench
-- exercise the runner's per-node facts; the canonical production schema is
-- deploy/sql/run-state.sql.
CREATE TABLE s3.runs (
    tenant_id       text NOT NULL CHECK (tenant_id <> ''),
    run_id          text NOT NULL,
    flow_id         text NOT NULL,
    flow_version    int  NOT NULL,
    status          text NOT NULL DEFAULT 'running'
        CHECK (status IN ('dispatched', 'running', 'completed', 'failed',
                          'infrastructure-failure', 'effect-uncertain')),
    trigger_source  text,
    capture_mode    text NOT NULL DEFAULT 'off'
        CHECK (capture_mode IN ('full', 'off')),
    input_json      jsonb,
    result_json     jsonb,
    state_json      jsonb,
    idempotency_key text,
    fail_kind       text,
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (capture_mode <> 'full' OR trigger_source IS NOT DISTINCT FROM 'scenario-draft'),
    PRIMARY KEY (tenant_id, run_id)
);
ALTER TABLE s3.runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE s3.runs FORCE ROW LEVEL SECURITY;
CREATE POLICY runs_tenant ON s3.runs
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY runs_platform ON s3.runs
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX runs_tkey
    ON s3.runs ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT, DELETE ON s3.runs TO wamn_app;
GRANT INSERT (
    tenant_id, run_id, flow_id, flow_version, status, trigger_source,
    input_json, result_json, state_json, idempotency_key,
    fail_kind, updated_at
), UPDATE (
    tenant_id, run_id, flow_id, flow_version, status, trigger_source,
    input_json, result_json, state_json, idempotency_key,
    fail_kind, updated_at
) ON s3.runs TO wamn_app;
