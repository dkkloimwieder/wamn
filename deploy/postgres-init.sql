-- S2 fixture: schema, RLS, seed data for the wamn:postgres plugin PoC.
-- Runs once at database init (docker-entrypoint-initdb.d locally, ConfigMap
-- mount in the kind cluster), as the postgres superuser.
--
-- Security shape under test: ONE application role (wamn_app, not owner, no
-- BYPASSRLS) and tenant separation purely via the `app.tenant` claim the
-- plugin injects with SET LOCAL. RLS policies key on
-- current_setting('app.tenant', true), which is NULL (=> zero rows) when no
-- claim was injected.

CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS;

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
    tenant_id text NOT NULL,
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
    USING (tenant_id = current_setting('app.tenant', true))
    WITH CHECK (tenant_id = current_setting('app.tenant', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON s2.bench TO wamn_app;

-- ---------------------------------------------------------------------------
-- RLS gate target: per-tenant secrets that must never cross tenants.
-- ---------------------------------------------------------------------------
CREATE TABLE s2.rls_secrets (
    id        bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id text NOT NULL,
    secret    text NOT NULL
);

INSERT INTO s2.rls_secrets (tenant_id, secret)
SELECT t.tenant, 'secret-' || t.tenant || '-' || gs
FROM generate_series(1, 1000) gs,
     (VALUES ('tenant-a'), ('tenant-b')) t(tenant);

ALTER TABLE s2.rls_secrets ENABLE ROW LEVEL SECURITY;
ALTER TABLE s2.rls_secrets FORCE ROW LEVEL SECURITY;
CREATE POLICY rls_secrets_tenant ON s2.rls_secrets
    USING (tenant_id = current_setting('app.tenant', true))
    WITH CHECK (tenant_id = current_setting('app.tenant', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON s2.rls_secrets TO wamn_app;

-- ---------------------------------------------------------------------------
-- Scratch table: chaos-gate transactions, injection round-trips, smoke tests.
-- One column per sql-value shape that needs a byte-identical round-trip.
-- ---------------------------------------------------------------------------
CREATE TABLE s2.scratch (
    id        bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id text NOT NULL,
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
    USING (tenant_id = current_setting('app.tenant', true))
    WITH CHECK (tenant_id = current_setting('app.tenant', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON s2.scratch TO wamn_app;

-- FK-violation fixture (FK checks run as table owner and bypass RLS; the
-- guest only needs a way to trip 23503).
CREATE TABLE s2.fkchild (
    id        bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id text NOT NULL,
    parent_id bigint NOT NULL CONSTRAINT fkchild_parent_fk REFERENCES s2.scratch (id)
);

ALTER TABLE s2.fkchild ENABLE ROW LEVEL SECURITY;
ALTER TABLE s2.fkchild FORCE ROW LEVEL SECURITY;
CREATE POLICY fkchild_tenant ON s2.fkchild
    USING (tenant_id = current_setting('app.tenant', true))
    WITH CHECK (tenant_id = current_setting('app.tenant', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON s2.fkchild TO wamn_app;

-- Identity columns: inserts by wamn_app need the backing sequences.
GRANT USAGE ON ALL SEQUENCES IN SCHEMA s2 TO wamn_app;
