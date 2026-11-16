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

CREATE SCHEMA catalog AUTHORIZATION CURRENT_USER;

DO $platform_group$ BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles
                 WHERE rolname = 'wamn_platform') THEN
    CREATE ROLE wamn_platform NOLOGIN NOSUPERUSER NOCREATEDB
      NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
  END IF;
EXCEPTION WHEN duplicate_object THEN NULL;
END $platform_group$;

-- Stable ACL role for record history retention (`wamn-emtx.13`). apply-package
-- grants it privileges on the history tables of this database, and
-- provisioning owns its targeted A/B LOGIN generations. It is not a wamn_platform
-- member.
DO $audit_retention$ BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles
                 WHERE rolname = 'wamn_audit_retention') THEN
    CREATE ROLE wamn_audit_retention NOLOGIN NOSUPERUSER NOCREATEDB
      NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
  END IF;
EXCEPTION WHEN duplicate_object THEN NULL;
END $audit_retention$;

DO $query_role$ BEGIN
    PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_query') THEN
        CREATE ROLE wamn_query NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE
            NOINHERIT NOREPLICATION NOBYPASSRLS;
    END IF;
END $query_role$;

GRANT USAGE ON SCHEMA catalog TO wamn_app;

CREATE FUNCTION catalog.reject_immutable_row_change()