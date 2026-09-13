-- Record history, level 1: the platform stamp trigger function.
--
-- This file carries no transaction of its own. CATALOG_SCHEMA_SQL composes it
-- inside the catalog bootstrap transaction. Other appliers wrap it in theirs.
--
-- wamn_history.stamp_row() is a BEFORE INSERT OR UPDATE row trigger function.
-- Its trigger arguments name the selected stamp columns, a subset of
-- created_at, created_by, updated_at, and updated_by. The actor is the bound
-- app.user_id and the time is transaction_timestamp(). A write with no bound
-- actor raises SQLSTATE 55000 with the message actor-required. The function
-- reads no users row and takes no authorization decision.

-- The file names wamn_db_owner, and a grant to a missing role fails the whole
-- apply. The attributes match ensure_db_owner_role_sql in provisioning.
DO $db_owner$ BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles
                 WHERE rolname = 'wamn_db_owner') THEN
    CREATE ROLE wamn_db_owner NOLOGIN NOSUPERUSER NOCREATEDB
      NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
  END IF;
EXCEPTION WHEN duplicate_object THEN NULL;
END $db_owner$;

-- Test setups drop catalog and apply CATALOG_SCHEMA_SQL again on the same
-- database. wamn_history stays, so the schema and function DDL is idempotent.
CREATE SCHEMA IF NOT EXISTS wamn_history;
REVOKE ALL ON SCHEMA wamn_history FROM PUBLIC;

CREATE OR REPLACE FUNCTION wamn_history.stamp_row()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $stamp_row$
DECLARE
    actor uuid := NULLIF(current_setting('app.user_id', true), '')::uuid;
    fresh jsonb;
    stamps jsonb;
BEGIN
    IF actor IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'actor-required';
    END IF;

    fresh := jsonb_build_object(
        'created_at', transaction_timestamp(),
        'created_by', actor,
        'updated_at', transaction_timestamp(),
        'updated_by', actor
    );
    IF TG_OP = 'INSERT' THEN
        stamps := fresh;
    ELSIF (to_jsonb(NEW) - TG_ARGV) IS DISTINCT FROM (to_jsonb(OLD) - TG_ARGV) THEN
        -- A changed row keeps the created pair and moves the updated pair.
        stamps := to_jsonb(OLD) || (fresh - 'created_at' - 'created_by');
    ELSE
        -- A true no-op keeps every OLD stamp.
        stamps := to_jsonb(OLD);
    END IF;

    -- Only the selected columns take a stamp. A supplied value is replaced.
    RETURN jsonb_populate_record(NEW, (
        SELECT jsonb_object_agg(key, value)
        FROM jsonb_each(stamps)
        WHERE key = ANY (TG_ARGV)
    ));
END
$stamp_row$;
REVOKE ALL ON FUNCTION wamn_history.stamp_row() FROM PUBLIC;

-- apply-package runs package DDL as wamn_db_owner and creates the triggers.
GRANT USAGE ON SCHEMA wamn_history TO wamn_db_owner;
GRANT EXECUTE ON FUNCTION wamn_history.stamp_row() TO wamn_db_owner;
