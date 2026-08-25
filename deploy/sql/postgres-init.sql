-- S2 fixture: schema, RLS, seed data for the wamn:postgres plugin PoC.
-- Runs once at database init (docker-entrypoint-initdb.d locally, ConfigMap
-- mount in the kind cluster), as the postgres superuser.
--
-- Security shape under test: ONE application role (wamn_app, not owner, no
-- BYPASSRLS) and tenant separation purely via the `app.tenant` claim the
-- plugin injects with SET LOCAL. RLS policies key on
-- NULLIF(current_setting('app.tenant', true), ''), which is NULL (=> zero rows)
-- when no claim was injected — Postgres resets a custom GUC to '' (not NULL)
-- after SET LOCAL, and CHECK (tenant_id <> '') forbids a ''-tenant row, so an
-- empty claim matches nothing structurally.

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

CREATE DATABASE wamn OWNER postgres;

\connect wamn

CREATE SCHEMA s2 AUTHORIZATION postgres;
GRANT USAGE ON SCHEMA s2 TO wamn_app;

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
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
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
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
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
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
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
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE, DELETE ON s2.fkchild TO wamn_app;

-- Identity columns: inserts by wamn_app need the backing sequences.
GRANT USAGE ON ALL SEQUENCES IN SCHEMA s2 TO wamn_app;

-- ===========================================================================
-- S3 fixture: flow catalog, production-shaped run history, and an idempotent
-- sink for the runner PoC (docs/archive/p0-exit-criteria.md S3). Same security
-- shape as s2: one app role, tenant separation via the app.tenant claim + RLS.
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
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
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
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
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
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
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
