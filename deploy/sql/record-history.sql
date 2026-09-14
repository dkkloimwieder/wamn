-- Record history: the platform stamp trigger function, the history table
-- function, and the log trigger function.
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
--
-- wamn_history.create_history_table(schema, relation, tenant) is the one
-- definition of the history table shape. wamn_history.row_image(record) is the
-- one rendering of a row image. wamn_history.log_row_change() is an AFTER
-- INSERT OR UPDATE OR DELETE row trigger that writes one entry into the history
-- table of the changed relation, in the same transaction.
--
-- Both trigger functions compare JSONB text, so a change of numeric scale alone
-- is a change.

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
    ELSIF (to_jsonb(NEW) - TG_ARGV)::text IS DISTINCT FROM (to_jsonb(OLD) - TG_ARGV)::text THEN
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

-- The history table <schema>.<relation>_history of one logged relation, with
-- the one fixed shape. The caller owns the table. CREATE TABLE IF NOT EXISTS
-- lets an applier run again. The function names every derived object, and it
-- refuses a relation when a derived name has 64 bytes or more, because
-- PostgreSQL truncates such a name. The longest derived name is the NOT NULL
-- constraint of transaction_id.
CREATE OR REPLACE FUNCTION wamn_history.create_history_table(
    schema text, relation text, tenant boolean)
RETURNS void
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $create_history_table$
DECLARE
    history text := relation || '_history';
    -- The catalog token grammar is <package-id>:<module>/<action>@<version>.
    -- The package id, the module, and the action are kebab identifiers. The
    -- version has no @, :, or /, and no Unicode white space at either end.
    kebab constant text := '[a-z][a-z0-9]*(-[a-z0-9]+)*';
    version_end constant text := E'[^@:/\\t\\n\\v\\f\\r \\u0085\\u00a0\\u1680'
        || E'\\u2000-\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000]';
    operation_pattern text := format(
        '^(%1$s:%1$s/%1$s@%2$s([^@:/]*%2$s)?|wamn:%1$s|admin:%1$s)$',
        kebab, version_end);
    columns text;
    derived text[];
BEGIN
    SELECT string_agg(format('%I %s CONSTRAINT %I NOT NULL', c.name, c.definition,
                             history || '_' || c.name || '_not_null'),
                      ', ' ORDER BY c.ord),
           array_agg(history || '_' || c.name || '_not_null')
      INTO columns, derived
      FROM (VALUES
            (1, 'position', format('bigint GENERATED ALWAYS AS IDENTITY (SEQUENCE NAME %I.%I)',
                                   schema, history || '_position_seq')),
            (2, 'tenant_id', 'text'),
            (3, 'row_key', 'jsonb'),
            (4, 'kind', 'text'),
            (5, 'operation', 'text'),
            (6, 'changed_by', 'uuid'),
            (7, 'changed_at', 'timestamptz'),
            (8, 'transaction_id', 'bigint'),
            (9, 'before', 'jsonb'),
            (10, 'after', 'jsonb')
           ) AS c (ord, name, definition)
     WHERE tenant OR c.name <> 'tenant_id';
    derived := derived || ARRAY[history || '_pkey', history || '_kind_check',
                                history || '_operation_check', history || '_position_seq'];
    IF EXISTS (SELECT FROM unnest(derived) AS d (name) WHERE octet_length(d.name) >= 64) THEN
        RAISE EXCEPTION USING ERRCODE = '22023',
            MESSAGE = format('history-name-too-long: %s', relation);
    END IF;

    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I.%I (%s, '
        'CONSTRAINT %I CHECK (kind IN (''insert'', ''update'', ''delete'')), '
        'CONSTRAINT %I CHECK (operation ~ %L), '
        'CONSTRAINT %I PRIMARY KEY (row_key, position))',
        schema, history, columns,
        history || '_kind_check',
        history || '_operation_check', operation_pattern,
        history || '_pkey');
END
$create_history_table$;
REVOKE ALL ON FUNCTION wamn_history.create_history_table(text, text, boolean) FROM PUBLIC;

-- The JSONB image of one row. to_jsonb spells a timestamptz with an offset and
-- trims trailing zeros. The function spells each finite timestamptz column as
-- the platform canonicalizer spells it: UTC RFC 3339 with exactly six
-- fractional digits and a Z. Every other value keeps the to_jsonb spelling.
CREATE OR REPLACE FUNCTION wamn_history.row_image(item record)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SET search_path = pg_catalog
SET TimeZone = 'UTC'
AS $row_image$
DECLARE
    image jsonb := to_jsonb(item);
BEGIN
    RETURN image || COALESCE((
        SELECT jsonb_object_agg(a.attname, to_char((image ->> a.attname)::timestamptz,
                                                   'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'))
          FROM pg_type AS t
          JOIN pg_attribute AS a ON a.attrelid = t.typrelid
         WHERE t.oid = pg_typeof(item) AND a.attnum > 0 AND NOT a.attisdropped
           AND a.atttypid = 'timestamptz'::regtype
           AND isfinite((image ->> a.attname)::timestamptz)), '{}');
END
$row_image$;
REVOKE ALL ON FUNCTION wamn_history.row_image(record) FROM PUBLIC;

-- The JSONB image of one timestamptz value, in the spelling of
-- wamn_history.row_image. A history read that names its columns builds the
-- current row image with this function and needs no whole-row reference.
CREATE OR REPLACE FUNCTION wamn_history.timestamptz_image(value timestamptz)
RETURNS jsonb
LANGUAGE sql
STABLE
SET search_path = pg_catalog
SET TimeZone = 'UTC'
AS $timestamptz_image$
    SELECT CASE WHEN isfinite(value)
                THEN to_jsonb(to_char(value, 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'))
                ELSE to_jsonb(value)
           END
$timestamptz_image$;
REVOKE ALL ON FUNCTION wamn_history.timestamptz_image(timestamptz) FROM PUBLIC;

-- The log trigger function. The trigger argument carries the retention, and
-- the function ignores it. The entry keys the row by its primary key columns.
-- A relation with no primary key raises SQLSTATE 55000 with the message
-- history-key-required. wamn_history.row_image renders both images.
CREATE OR REPLACE FUNCTION wamn_history.log_row_change()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $log_row_change$
DECLARE
    actor uuid := NULLIF(current_setting('app.user_id', true), '')::uuid;
    operation text := NULLIF(current_setting('app.operation', true), '');
    history text := format('%I.%I', TG_TABLE_SCHEMA, TG_TABLE_NAME || '_history');
    old_row jsonb := COALESCE(wamn_history.row_image(OLD), '{}');
    new_row jsonb := COALESCE(wamn_history.row_image(NEW), '{}');
    key_columns text[];
    old_key jsonb;
    new_key jsonb;
    changed_before jsonb;
    changed_after jsonb;
    tenant boolean;
    write_entry text;
BEGIN
    IF actor IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'actor-required';
    END IF;
    IF operation IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'operation-required';
    END IF;

    SELECT array_agg(a.attname::text) INTO key_columns
      FROM pg_index AS i
      JOIN pg_attribute AS a ON a.attrelid = i.indrelid AND a.attnum = ANY (i.indkey)
     WHERE i.indrelid = TG_RELID AND i.indisprimary;
    IF key_columns IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'history-key-required';
    END IF;

    -- A true no-op writes no entry.
    IF old_row::text = new_row::text THEN
        RETURN NULL;
    END IF;

    old_key := (SELECT jsonb_object_agg(k, old_row -> k) FROM unnest(key_columns) AS k);
    new_key := (SELECT jsonb_object_agg(k, new_row -> k) FROM unnest(key_columns) AS k);
    -- Only a history table under a tenant floor has tenant_id. The entry copies
    -- it from the row.
    tenant := EXISTS (SELECT FROM pg_attribute
                       WHERE attrelid = to_regclass(history) AND attname = 'tenant_id'
                         AND NOT attisdropped);
    write_entry := format(
        'INSERT INTO %s (%s row_key, kind, operation, changed_by, changed_at, '
        'transaction_id, before, after) VALUES (%s $2, $3, $4, $5, '
        'transaction_timestamp(), pg_current_xact_id()::text::bigint, $6, $7)',
        history,
        CASE WHEN tenant THEN 'tenant_id,' ELSE '' END,
        CASE WHEN tenant THEN '$1,' ELSE '' END);

    IF TG_OP = 'UPDATE' AND old_key = new_key THEN
        -- An update records the prior and the resulting values of the changed
        -- columns.
        SELECT jsonb_object_agg(o.key, o.value), jsonb_object_agg(o.key, new_row -> o.key)
          INTO changed_before, changed_after
          FROM jsonb_each(old_row) AS o
         WHERE o.value::text IS DISTINCT FROM (new_row -> o.key)::text;
        EXECUTE write_entry USING new_row ->> 'tenant_id', new_key, 'update'::text,
            operation, actor, changed_before, changed_after;
        RETURN NULL;
    END IF;

    -- An insert and a delete record the full row. A primary key change records
    -- a delete under the old key and an insert under the new key.
    IF TG_OP <> 'INSERT' THEN
        EXECUTE write_entry USING old_row ->> 'tenant_id', old_key, 'delete'::text,
            operation, actor, old_row, '{}'::jsonb;
    END IF;
    IF TG_OP <> 'DELETE' THEN
        EXECUTE write_entry USING new_row ->> 'tenant_id', new_key, 'insert'::text,
            operation, actor, '{}'::jsonb, new_row;
    END IF;
    RETURN NULL;
END
$log_row_change$;
REVOKE ALL ON FUNCTION wamn_history.log_row_change() FROM PUBLIC;

-- apply-package runs package DDL as wamn_db_owner. It creates the history
-- tables and the triggers. CREATE TRIGGER needs EXECUTE on the trigger
-- function, and a trigger that fires needs no EXECUTE, so wamn_app gets none.
-- The log function calls wamn_history.row_image with the authority of the
-- writer, so every writer of a logged relation needs EXECUTE on it.
GRANT USAGE ON SCHEMA wamn_history TO wamn_db_owner;
GRANT EXECUTE ON FUNCTION wamn_history.stamp_row() TO wamn_db_owner;
GRANT EXECUTE ON FUNCTION wamn_history.create_history_table(text, text, boolean)
    TO wamn_db_owner;
GRANT EXECUTE ON FUNCTION wamn_history.row_image(record) TO wamn_db_owner;
GRANT EXECUTE ON FUNCTION wamn_history.timestamptz_image(timestamptz) TO wamn_db_owner;
GRANT EXECUTE ON FUNCTION wamn_history.log_row_change() TO wamn_db_owner;

-- wamn_app writes logged relations, and a history read renders the current row
-- through wamn_history.row_image or wamn_history.timestamptz_image. The system
-- database has no wamn_app, so an applier without that role skips these grants.
DO $wamn_app$ BEGIN
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_app') THEN
    GRANT USAGE ON SCHEMA wamn_history TO wamn_app;
    GRANT EXECUTE ON FUNCTION wamn_history.row_image(record) TO wamn_app;
    GRANT EXECUTE ON FUNCTION wamn_history.timestamptz_image(timestamptz) TO wamn_app;
  END IF;
END $wamn_app$;
