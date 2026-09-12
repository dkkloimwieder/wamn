-- Project-environment package, release, wiring, and registration storage.
--
-- Package directories are the authoring truth: raw `wamn.json` bytes identify
-- an immutable package coordinate and ordered migration bytes are recorded in
-- the same transaction that applies them. Effective releases independently
-- compose exact package coordinates. No catalog-document, schema-model, or
-- generated semantic-table registry survives this schema.
--
-- STANDALONE TRANSACTIONAL ARTIFACT. This file is not included by
-- postgres-init.sql; the project-environment provisioner applies it whole.

BEGIN;

CREATE SCHEMA catalog AUTHORIZATION postgres;

DO $platform_group$ BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles
                 WHERE rolname = 'wamn_platform') THEN
    CREATE ROLE wamn_platform NOLOGIN NOSUPERUSER NOCREATEDB
      NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
  END IF;
EXCEPTION WHEN duplicate_object THEN NULL;
END $platform_group$;

GRANT USAGE ON SCHEMA catalog TO wamn_app;
GRANT USAGE ON SCHEMA catalog TO wamn_scenario_author;

-- GENERATED authority-derivation bootstrap. The database name is embedded so
-- tenant_key stays IMMUTABLE and remains legal in expression indexes.
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

CREATE FUNCTION catalog.reject_immutable_row_change()