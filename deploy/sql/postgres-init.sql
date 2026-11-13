-- S2 fixture: schema, RLS, seed data for the wamn:postgres plugin PoC.
-- Runs once at database init (docker-entrypoint-initdb.d locally, ConfigMap
-- mount in the kind cluster), as the postgres superuser.
--
-- Security shape under test: ONE stable ACL role (wamn_app, not owner, no
-- BYPASSRLS) and project separation from `current_user` (wamn-0h0g.22.6). RLS
-- policies key on
--   wamn_authority.project_key(project_id) = wamn_authority.current_project_key()
-- so a session that can set its own identity gains nothing: the connected role is
-- the one thing it cannot rewrite. A role outside the guest generation
-- convention derives NULL and matches no row, and CHECK (project_id <> '')
-- forbids a ''-project row, so the minimum is structural rather than conventional.
--
-- `wamn-0h0g.22.17` gives every governed relation TWO arms: the project base grants,
-- narrowed `TO wamn_app`, and one permissive `TO wamn_platform` arm for the
-- platform-grain principals whose login names carry no project at all.

-- GUARDED, like every other role in this file (`wamn-0h0g.12.186`). It was a
-- BARE `CREATE ROLE` and roles are CLUSTER-global, so re-applying this artifact
-- against a cluster that already carries the role failed the WHOLE FILE at its
-- first statement with `role "wamn_app" already exists` — measured. Nothing
-- downstream of line 18 ran, which is why
-- `crates/control/provision/tests/deploy_sql_authority.rs` reads 4 passed / 1
-- failed under parallel test threads and 5/0 single-threaded.
--
-- EXCEPTION-guarded and advisory-locked, not bare `IF NOT EXISTS`: two appliers
-- that do not both take the lock can each observe the role absent and both
-- issue `CREATE ROLE`, and the loser gets `duplicate_object`.
--
-- `wamn-0h0g.12.140`: this is a stable ACL and grant-owner role, never a
-- credential. Guest sessions authenticate as targeted `wamn_app_<target>_[ab]`
-- generations that inherit it. Both a fresh install and a replay over the old
-- shared LOGIN therefore converge to NOLOGIN, PASSWORD NULL and NOINHERIT.
DO $app$ BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_app') THEN
    CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE
      NOINHERIT NOREPLICATION NOBYPASSRLS;
  ELSIF EXISTS (SELECT FROM pg_catalog.pg_authid WHERE rolname = 'wamn_app'
                AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole
                     OR rolinherit OR rolreplication OR rolbypassrls
                     OR rolpassword IS NOT NULL)) THEN
    ALTER ROLE wamn_app NOLOGIN PASSWORD NULL NOSUPERUSER NOCREATEDB
      NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
  END IF;
EXCEPTION WHEN duplicate_object THEN
  ALTER ROLE wamn_app NOLOGIN PASSWORD NULL NOSUPERUSER NOCREATEDB
    NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
END $app$;

-- psql commits the preceding DO statement before executing this one: NOLOGIN
-- is visible cluster-wide before the scan, so a new shared-role session cannot
-- race the drain. The native timeout bounds each wait; residue is a hard error.
DO $app_drain$ DECLARE stale_pid integer; BEGIN
  FOR stale_pid IN
    SELECT pid FROM pg_catalog.pg_stat_activity
    WHERE usename = 'wamn_app' AND pid <> pg_backend_pid()
  LOOP
    PERFORM pg_catalog.pg_terminate_backend(stale_pid, 5000);
  END LOOP;
  PERFORM pg_catalog.pg_stat_clear_snapshot();
  IF EXISTS (
    SELECT FROM pg_catalog.pg_stat_activity
    WHERE usename = 'wamn_app' AND pid <> pg_backend_pid()
  ) THEN
    RAISE EXCEPTION USING
      ERRCODE = '55000',
      MESSAGE = 'wamn-app-session-drain-incomplete';
  END IF;
END $app_drain$;

-- Stable host-only ACL role for run-history retention (`wamn-0h0g.12.69`). Its
-- targeted A/B LOGIN generations, their CONNECT and their Secret are provisioned
-- separately; this local fixture needs only the NOLOGIN grant carrier, so
-- canonical run-state.sql can be applied against it.
DO $run_retention$ BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles
                 WHERE rolname = 'wamn_run_retention') THEN
    CREATE ROLE wamn_run_retention NOLOGIN NOSUPERUSER NOCREATEDB
      NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
  END IF;
EXCEPTION WHEN duplicate_object THEN NULL;
END $run_retention$;

-- The shared platform group role every non-guest base policy arm targets
-- (`wamn-0h0g.22.17`). PostgreSQL DEFAULT-DENIES when RLS is enabled and no
-- policy matches the connected role, so narrowing the minimum `TO wamn_app` does
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
-- Every role below is NOINHERIT, and in PostgreSQL 16+ a role's `rolinherit`
-- supplies the DEFAULT `INHERIT` option for memberships granted TO it — so a
-- bare `GRANT wamn_platform TO wamn_run_retention` lands `inherit_option =
-- false`, the two-hop chain (generation login -> ACL role -> wamn_platform)
-- dies, and the platform principal reads ZERO ROWS with no error at all.
-- Measured on PostgreSQL 18.6: bare grant -> 0 rows, `INHERIT TRUE` -> all rows.
--
GRANT wamn_platform TO wamn_run_retention WITH ADMIN FALSE, INHERIT TRUE, SET FALSE;

CREATE DATABASE wamn;

\connect wamn

CREATE SCHEMA s2 AUTHORIZATION CURRENT_USER;
GRANT USAGE ON SCHEMA s2 TO wamn_app;

-- ---------------------------------------------------------------------------
-- The per-database authority derivations every project policy below calls
-- (`wamn-0h0g.22.6`). Guest project authority comes from `current_user`, not
-- from an identity the session can set: a session that can set its own project can
-- read every project.
--
-- GENERATED — this block is `authority_derivations_bootstrap_sql()` verbatim,
-- pinned by a byte-equality guard in wamn-control-provision. Do not hand-edit;
-- change the builder.
--
-- It is the BOOTSTRAP rendering because this file is applied to databases whose
-- names are not known here, and `project_key` must bake the name in as a literal
-- to stay `IMMUTABLE` — without which the expression indexes below are not even
-- creatable.
-- ---------------------------------------------------------------------------
DO $wamn_authority_bootstrap$
BEGIN
    EXECUTE replace(replace($wamn_authority_derivations$CREATE SCHEMA IF NOT EXISTS "wamn_authority" AUTHORIZATION CURRENT_USER;
REVOKE ALL ON SCHEMA "wamn_authority" FROM PUBLIC;
GRANT USAGE ON SCHEMA "wamn_authority" TO "wamn_app";
CREATE OR REPLACE FUNCTION "wamn_authority".project_key(project_env_key text)
RETURNS text
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
STRICT
SET search_path = pg_catalog
AS $$
    SELECT substr(encode(sha256(
           int8send(22::bigint) || convert_to('wamn.app.identity.v0.1', 'UTF8')
        || int8send(15::bigint) || convert_to('project_env_key', 'UTF8')
        || int8send(octet_length(convert_to(project_env_key, 'UTF8'))::bigint) || convert_to(project_env_key, 'UTF8')
        || int8send(8::bigint) || convert_to('database', 'UTF8')
        || int8send(@wamn_database_octets@::bigint) || convert_to(@wamn_database_literal@, 'UTF8')
       ), 'hex'), 1, 40)
$$;
ALTER FUNCTION "wamn_authority".project_key(text) OWNER TO CURRENT_USER;
REVOKE ALL ON FUNCTION "wamn_authority".project_key(text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION "wamn_authority".project_key(text) TO "wamn_app";
CREATE OR REPLACE FUNCTION "wamn_authority".current_project_key()
RETURNS text
LANGUAGE sql
STABLE
PARALLEL SAFE
SET search_path = pg_catalog
AS $$
    SELECT substring(current_user::text from '^wamn_app_([0-9a-f]{40})_[ab]$')
$$;
ALTER FUNCTION "wamn_authority".current_project_key() OWNER TO CURRENT_USER;
REVOKE ALL ON FUNCTION "wamn_authority".current_project_key() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION "wamn_authority".current_project_key() TO "wamn_app";$wamn_authority_derivations$,
        '@wamn_database_literal@', quote_literal(current_database())),
        '@wamn_database_octets@', octet_length(convert_to(current_database(), 'UTF8'))::text);
END
$wamn_authority_bootstrap$;

-- ---------------------------------------------------------------------------
-- Bench target: single-statement query with 8 params returning 10 rows.
-- 20 rows per (project, g) group so LIMIT 10 always has headroom.
-- ---------------------------------------------------------------------------
CREATE TABLE s2.bench (
    id      bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    project_id text NOT NULL CHECK (project_id <> ''),
    g       int NOT NULL,
    a       int NOT NULL,
    b       bigint NOT NULL,
    c       double precision NOT NULL,
    num     numeric(12,4) NOT NULL,
    ts      timestamptz NOT NULL,
    payload text NOT NULL
);

INSERT INTO s2.bench (project_id, g, a, b, c, num, ts, payload)
SELECT t.project,
       gs % 1000,
       gs % 100,
       gs::bigint * 1000,
       gs::double precision / 3.0,
       ((gs % 100000)::numeric) / 100,
       TIMESTAMPTZ '2026-01-01 00:00:00+00' + (gs % 86400) * INTERVAL '1 second',
       'payload-' || gs
FROM generate_series(1, 20000) gs,
     (VALUES ('org-a'), ('org-b')) t(project);

CREATE INDEX bench_project_g_id ON s2.bench (project_id, g, id);

ALTER TABLE s2.bench ENABLE ROW LEVEL SECURITY;
ALTER TABLE s2.bench FORCE ROW LEVEL SECURITY;
CREATE POLICY bench_project ON s2.bench
    TO wamn_app
    USING (wamn_authority.project_key(project_id) = wamn_authority.current_project_key())
    WITH CHECK (wamn_authority.project_key(project_id) = wamn_authority.current_project_key());
CREATE POLICY bench_platform ON s2.bench
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX bench_tkey
    ON s2.bench ((wamn_authority.project_key(project_id)));
GRANT SELECT, INSERT, UPDATE, DELETE ON s2.bench TO wamn_app;

-- ---------------------------------------------------------------------------
-- RLS condition target: per-project secrets that must never cross projects.
-- ---------------------------------------------------------------------------
CREATE TABLE s2.rls_secrets (
    id        bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    project_id text NOT NULL CHECK (project_id <> ''),
    secret    text NOT NULL
);

INSERT INTO s2.rls_secrets (project_id, secret)
SELECT t.project, 'secret-' || t.project || '-' || gs
FROM generate_series(1, 1000) gs,
     (VALUES ('org-a'), ('org-b')) t(project);

ALTER TABLE s2.rls_secrets ENABLE ROW LEVEL SECURITY;
ALTER TABLE s2.rls_secrets FORCE ROW LEVEL SECURITY;
CREATE POLICY rls_secrets_project ON s2.rls_secrets
    TO wamn_app
    USING (wamn_authority.project_key(project_id) = wamn_authority.current_project_key())
    WITH CHECK (wamn_authority.project_key(project_id) = wamn_authority.current_project_key());
CREATE POLICY rls_secrets_platform ON s2.rls_secrets
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX rls_secrets_tkey
    ON s2.rls_secrets ((wamn_authority.project_key(project_id)));
GRANT SELECT, INSERT, UPDATE, DELETE ON s2.rls_secrets TO wamn_app;

-- ---------------------------------------------------------------------------
-- Scratch table: chaos-acceptance-test transactions, injection round-trips, smoke tests.
-- One column per sql-value shape that needs a byte-identical round-trip.
-- ---------------------------------------------------------------------------
CREATE TABLE s2.scratch (
    id        bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    project_id text NOT NULL CHECK (project_id <> ''),
    k         text NOT NULL,
    v         text,
    vb        bytea,
    vn        numeric,
    vts       timestamptz,
    vj        jsonb,
    CONSTRAINT scratch_project_k_uniq UNIQUE (project_id, k),
    CONSTRAINT scratch_k_check CHECK (k <> 'forbidden')
);

ALTER TABLE s2.scratch ENABLE ROW LEVEL SECURITY;
ALTER TABLE s2.scratch FORCE ROW LEVEL SECURITY;
CREATE POLICY scratch_project ON s2.scratch
    TO wamn_app
    USING (wamn_authority.project_key(project_id) = wamn_authority.current_project_key())
    WITH CHECK (wamn_authority.project_key(project_id) = wamn_authority.current_project_key());
CREATE POLICY scratch_platform ON s2.scratch
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX scratch_tkey
    ON s2.scratch ((wamn_authority.project_key(project_id)));
GRANT SELECT, INSERT, UPDATE, DELETE ON s2.scratch TO wamn_app;

-- FK-violation fixture (FK tests run as table owner and bypass RLS; the
-- guest only needs a way to trip 23503).
CREATE TABLE s2.fkchild (
    id        bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    project_id text NOT NULL CHECK (project_id <> ''),
    parent_id bigint NOT NULL CONSTRAINT fkchild_parent_fk REFERENCES s2.scratch (id)
);

ALTER TABLE s2.fkchild ENABLE ROW LEVEL SECURITY;
ALTER TABLE s2.fkchild FORCE ROW LEVEL SECURITY;
CREATE POLICY fkchild_project ON s2.fkchild
    TO wamn_app
    USING (wamn_authority.project_key(project_id) = wamn_authority.current_project_key())
    WITH CHECK (wamn_authority.project_key(project_id) = wamn_authority.current_project_key());
CREATE POLICY fkchild_platform ON s2.fkchild
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX fkchild_tkey
    ON s2.fkchild ((wamn_authority.project_key(project_id)));
GRANT SELECT, INSERT, UPDATE, DELETE ON s2.fkchild TO wamn_app;

-- Identity columns: inserts by wamn_app need the backing sequences.
GRANT USAGE ON ALL SEQUENCES IN SCHEMA s2 TO wamn_app;
