-- Model versions: one counter for each model relation of a project database.
--
-- This file carries no transaction of its own. CATALOG_SCHEMA_SQL composes it
-- after record-history-app-grants.sql. It names wamn_db_owner, which
-- record-history.sql creates.
--
-- wamn_cache.model_versions holds one row for each relation that a write
-- changed. A relation with no row has version 0. A read of a query or a
-- projection takes its weak ETag from the versions of the relations that it
-- reads (docs/architecture/execution.md).
--
-- apply-package installs two triggers on each relation that a package owns.
-- The statement trigger wamn_cache_note records the changed relation in the
-- transaction setting wamn_cache.changed and takes no lock. The deferred
-- constraint trigger wamn_cache_bump fires at commit. Its first firing adds 1
-- to the version of every recorded relation in name order and clears the
-- setting, so the later firings do nothing. A transaction therefore adds 1 to
-- each relation that it changed, once. It locks the version rows in one total
-- order and holds them only through the commit, so two writers cannot
-- deadlock on them. A rolled-back write or savepoint rolls back its record
-- and its bump.
--
-- A TRUNCATE fires no row trigger, so the statement trigger adds 1 at once.
-- A TRUNCATE already holds an exclusive lock on its relation.
--
-- Both functions run as their owner. wamn_app holds no privilege on
-- wamn_cache, so a guest cannot write a version directly.

CREATE SCHEMA IF NOT EXISTS wamn_cache;
REVOKE ALL ON SCHEMA wamn_cache FROM PUBLIC;

CREATE TABLE IF NOT EXISTS wamn_cache.model_versions (
    schema_name text NOT NULL,
    relation_name text NOT NULL,
    version bigint NOT NULL CHECK (version > 0),
    PRIMARY KEY (schema_name, relation_name)
);
REVOKE ALL ON wamn_cache.model_versions FROM PUBLIC;

CREATE OR REPLACE FUNCTION wamn_cache.bump_version(schema_name text, relation_name text)
RETURNS void
LANGUAGE sql
SECURITY DEFINER
SET search_path = pg_catalog
AS $bump_version$
    INSERT INTO wamn_cache.model_versions AS current (schema_name, relation_name, version)
    VALUES ($1, $2, 1)
    ON CONFLICT ON CONSTRAINT model_versions_pkey
    DO UPDATE SET version = current.version + 1;
$bump_version$;
REVOKE ALL ON FUNCTION wamn_cache.bump_version(text, text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION wamn_cache.note_change()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $note_change$
DECLARE
    relation text := format('%I.%I', TG_TABLE_SCHEMA, TG_TABLE_NAME);
    changed jsonb := COALESCE(
        NULLIF(current_setting('wamn_cache.changed', true), '')::jsonb,
        '{}'
    );
BEGIN
    IF TG_OP = 'TRUNCATE' THEN
        PERFORM wamn_cache.bump_version(TG_TABLE_SCHEMA, TG_TABLE_NAME);
    ELSIF NOT changed ? relation THEN
        PERFORM set_config(
            'wamn_cache.changed',
            (changed || jsonb_build_object(
                relation, jsonb_build_array(TG_TABLE_SCHEMA, TG_TABLE_NAME)
            ))::text,
            true
        );
    END IF;
    RETURN NULL;
END;
$note_change$;
REVOKE ALL ON FUNCTION wamn_cache.note_change() FROM PUBLIC;

CREATE OR REPLACE FUNCTION wamn_cache.bump_changed()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $bump_changed$
DECLARE
    changed jsonb := NULLIF(current_setting('wamn_cache.changed', true), '')::jsonb;
    relation jsonb;
BEGIN
    IF changed IS NULL THEN
        RETURN NULL;
    END IF;
    PERFORM set_config('wamn_cache.changed', '', true);
    FOR relation IN
        SELECT entry.value FROM jsonb_each(changed) AS entry ORDER BY entry.key COLLATE "C"
    LOOP
        PERFORM wamn_cache.bump_version(relation ->> 0, relation ->> 1);
    END LOOP;
    RETURN NULL;
END;
$bump_changed$;
REVOKE ALL ON FUNCTION wamn_cache.bump_changed() FROM PUBLIC;

-- apply-package creates the triggers as wamn_db_owner, the owner of the
-- package relations.
GRANT USAGE ON SCHEMA wamn_cache TO wamn_db_owner;
GRANT EXECUTE ON FUNCTION wamn_cache.note_change() TO wamn_db_owner;
GRANT EXECUTE ON FUNCTION wamn_cache.bump_changed() TO wamn_db_owner;
