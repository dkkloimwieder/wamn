-- The database is the org boundary (owner ruling of 2026-10-23 on
-- wamn-83eth.6): one database for each (org, project, environment). This file
-- drops the tenant floor from platform."user" and from the platform
-- configuration tables in an installed project database: external_system,
-- connection, integration_allow, project_connection, number_sequence,
-- number_counter and setting, and the history table of each. Each loses
-- tenant_id, its row policies, its tenant_key index and its row security.
-- Each primary key, unique key and foreign key loses its tenant_id column.
-- No alias or view keeps the old shape.
--
-- user_role and api_key keep tenant_id. Their foreign key to platform."user"
-- becomes the one column user_id. Each history entry keeps the row images it
-- has, and its row_key loses tenant_id, so the history of a row stays under
-- the key of the row. A key or name that two tenants share in one database
-- makes the new primary or unique key fail, and the file then changes
-- nothing. upgrade-schema runs this file once in one transaction and records
-- it. A fresh install records it as applied, because app-schema.sql already
-- has the new shape.
--
-- Then comes the NUMBER COUNTERS section of deploy/sql/app-schema.sql, byte
-- for byte: bump_counter, assign_counter and set_counter check the
-- app.application claim and no tenant. The rest is the output of
-- wamn_control_provision::sql::grant_administration_surface_sql("wamn_run"),
-- the latest rendering of the administration surface, which grants no
-- tenant_id on the history tables of these tables. A test in
-- crates/control/provision/src/schema_migrations.rs compares both. 0003,
-- 0006, 0008, 0016, 0020, 0024, 0026, 0027 and 0028 hold the earlier surfaces.

-- The foreign keys that name tenant_id on the side of a key that changes.
DO $tenant_foreign_keys$ DECLARE target record; BEGIN
  FOR target IN
    SELECT c.conrelid::regclass AS relation, c.conname
      FROM pg_catalog.pg_constraint c
     WHERE c.contype = 'f'
       AND c.conrelid IN ('platform.user_role'::regclass, 'platform.api_key'::regclass,
                          'platform.connection'::regclass, 'platform.project_connection'::regclass,
                          'platform.number_counter'::regclass)
       AND c.confrelid IN ('platform."user"'::regclass, 'platform.external_system'::regclass,
                           'platform.connection'::regclass, 'platform.number_sequence'::regclass)
  LOOP
    EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I', target.relation, target.conname);
  END LOOP;
END $tenant_foreign_keys$;

-- Each table and its history table: no row security, no policy, no
-- tenant_key index and no tenant_id. Dropping the column drops each key and
-- check that names it.
DO $org_boundary$ DECLARE relation text; history boolean; BEGIN
  FOR relation, history IN
    SELECT r.name, r.history
      FROM (VALUES ('user', true), ('external_system', true), ('connection', true),
                   ('integration_allow', true), ('project_connection', true),
                   ('number_sequence', true), ('number_counter', false), ('setting', true))
        AS r (name, history)
  LOOP
    EXECUTE format('DROP POLICY %I ON platform.%I', relation || '_tenant', relation);
    EXECUTE format('DROP POLICY %I ON platform.%I', relation || '_platform', relation);
    EXECUTE format('DROP INDEX platform.%I', relation || '_tkey');
    EXECUTE format('ALTER TABLE platform.%I NO FORCE ROW LEVEL SECURITY', relation);
    EXECUTE format('ALTER TABLE platform.%I DISABLE ROW LEVEL SECURITY', relation);
    EXECUTE format('ALTER TABLE platform.%I DROP COLUMN tenant_id', relation);
    IF history THEN
      EXECUTE format('DROP POLICY %I ON platform.%I', relation || '_history_tenant',
                     relation || '_history');
      EXECUTE format('DROP POLICY %I ON platform.%I', relation || '_history_platform',
                     relation || '_history');
      EXECUTE format('DROP INDEX platform.%I', relation || '_history_tkey');
      EXECUTE format('ALTER TABLE platform.%I NO FORCE ROW LEVEL SECURITY', relation || '_history');
      EXECUTE format('ALTER TABLE platform.%I DISABLE ROW LEVEL SECURITY', relation || '_history');
      EXECUTE format('ALTER TABLE platform.%I DROP COLUMN tenant_id', relation || '_history');
      EXECUTE format('UPDATE platform.%I SET row_key = row_key - ''tenant_id'' '
                     'WHERE row_key ? ''tenant_id''', relation || '_history');
    END IF;
  END LOOP;
END $org_boundary$;

-- The keys of app-schema.sql, with the names PostgreSQL gives them there.
ALTER TABLE platform."user" ADD CONSTRAINT user_pkey PRIMARY KEY (id);
ALTER TABLE platform."user" ADD CONSTRAINT user_email_key UNIQUE (email);
ALTER TABLE platform.user_role ADD CONSTRAINT user_role_user_id_fkey
    FOREIGN KEY (user_id) REFERENCES platform."user" (id) ON DELETE CASCADE;
ALTER TABLE platform.api_key ADD CONSTRAINT api_key_user_id_fkey
    FOREIGN KEY (user_id) REFERENCES platform."user" (id) ON DELETE CASCADE;
ALTER TABLE platform.external_system ADD CONSTRAINT external_system_pkey PRIMARY KEY (id);
ALTER TABLE platform.external_system ADD CONSTRAINT external_system_name_key UNIQUE (name);
ALTER TABLE platform.connection ADD CONSTRAINT connection_pkey PRIMARY KEY (id);
ALTER TABLE platform.connection ADD CONSTRAINT connection_name_key UNIQUE (name);
ALTER TABLE platform.connection ADD CONSTRAINT connection_external_system_id_fkey
    FOREIGN KEY (external_system_id) REFERENCES platform.external_system (id);
ALTER TABLE platform.integration_allow ADD CONSTRAINT integration_allow_pkey
    PRIMARY KEY (scope_type, scope_id, integration);
ALTER TABLE platform.project_connection ADD CONSTRAINT project_connection_pkey
    PRIMARY KEY (project_id, connection_id);
ALTER TABLE platform.project_connection ADD CONSTRAINT project_connection_connection_id_fkey
    FOREIGN KEY (connection_id) REFERENCES platform.connection (id);
ALTER TABLE platform.number_sequence ADD CONSTRAINT number_sequence_pkey PRIMARY KEY (id);
ALTER TABLE platform.number_sequence ADD CONSTRAINT number_sequence_application_name_key
    UNIQUE (application, name);
ALTER TABLE platform.number_counter ADD CONSTRAINT number_counter_pkey
    PRIMARY KEY (sequence_id, counter, key_value);
ALTER TABLE platform.number_counter ADD CONSTRAINT number_counter_sequence_id_fkey
    FOREIGN KEY (sequence_id) REFERENCES platform.number_sequence (id) ON DELETE CASCADE;
ALTER TABLE platform.setting ADD CONSTRAINT setting_pkey PRIMARY KEY (scope_type, scope_id, key);

-- ---------------------------------------------------------------------------
-- NUMBER COUNTERS (docs/plan/platform-config.md, Numbering and
-- Administration; owner rulings of wamn-83eth.2, wamn-83eth.3 and
-- wamn-83eth.6). The three platform functions that write number_counter.
-- Each runs as its owner, the applier, so that wamn_app needs no write on
-- number_counter. None knows a pattern or parses anything; the renderer of
-- the wamn:postgres plugin owns both.
--
-- bump_counter advances one counter and returns the value to render: it
-- locks the counter row, inserts it at 1 when it is absent, returns its value
-- and increments it.
--
-- assign_counter gives a per= counter one value for each distinct key: under
-- a transaction advisory lock on the (sequence, counter, key), it returns the
-- value already assigned to the key, or allocates the next value from the
-- counter's '' allocator row and inserts the key row with it. Concurrent
-- first callers of one key wait on the lock, so the allocator advances once
-- for each key, and a rollback gives the value back. The '' key value is the
-- allocator itself, so it is refused with invalid_parameter_value.
--
-- set_counter is the write of the set_counter host route: under the counter
-- row's lock, it inserts the row of (counter, key) with the value, or sets
-- the value of the row that exists. The value is the next value to render. A
-- value below 1 is refused with invalid_parameter_value. The route refuses a
-- per= counter and a set below a live number with the renderer before it
-- commits.
--
-- The database is the org boundary, so no function checks a tenant. The
-- app.application claim of the operation transaction decides which sequences
-- a caller reaches: a sequence of another application, or a caller with no
-- application claim, is refused with undefined_object, the refusal of the
-- renderer for a sequence that the claim does not own.
--
-- CREATE OR REPLACE, so that project migration 0031 replaces the functions
-- of 0025, 0029 and 0030 in an installed database. 0031 carries this section
-- verbatim, and a test in crates/control/provision/src/schema_migrations.rs
-- compares the bytes.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION platform.bump_counter(target_sequence uuid, target_counter text,
                                                 target_key text)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
DECLARE
    allocated bigint;
BEGIN
    IF NOT EXISTS (SELECT FROM platform.number_sequence AS s
                    WHERE s.id = target_sequence
                      AND s.application = NULLIF(current_setting('app.application', true), '')) THEN
        RAISE EXCEPTION 'number sequence % is not a sequence of the claimed application', target_sequence
            USING ERRCODE = 'undefined_object';
    END IF;
    INSERT INTO platform.number_counter AS c (sequence_id, counter, key_value, value)
    VALUES (target_sequence, target_counter, target_key, 2)
    ON CONFLICT (sequence_id, counter, key_value)
    DO UPDATE SET value = c.value + 1
    RETURNING c.value - 1 INTO allocated;
    RETURN allocated;
END;
$$;
REVOKE ALL ON FUNCTION platform.bump_counter(uuid, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.bump_counter(uuid, text, text) TO wamn_app;

CREATE OR REPLACE FUNCTION platform.assign_counter(target_sequence uuid, target_counter text,
                                                   target_key text)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
DECLARE
    assigned bigint;
BEGIN
    IF NOT EXISTS (SELECT FROM platform.number_sequence AS s
                    WHERE s.id = target_sequence
                      AND s.application = NULLIF(current_setting('app.application', true), '')) THEN
        RAISE EXCEPTION 'number sequence % is not a sequence of the claimed application', target_sequence
            USING ERRCODE = 'undefined_object';
    END IF;
    IF target_key = '' THEN
        RAISE EXCEPTION 'the '''' key value is the allocator row of counter %', target_counter
            USING ERRCODE = 'invalid_parameter_value';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended(
        format('number_counter %s %s %s', target_sequence, target_counter, target_key), 0));
    SELECT c.value INTO assigned
      FROM platform.number_counter AS c
     WHERE c.sequence_id = target_sequence
       AND c.counter = target_counter
       AND c.key_value = target_key;
    IF assigned IS NOT NULL THEN
        RETURN assigned;
    END IF;
    INSERT INTO platform.number_counter AS c (sequence_id, counter, key_value, value)
    VALUES (target_sequence, target_counter, '', 2)
    ON CONFLICT (sequence_id, counter, key_value)
    DO UPDATE SET value = c.value + 1
    RETURNING c.value - 1 INTO assigned;
    INSERT INTO platform.number_counter (sequence_id, counter, key_value, value)
    VALUES (target_sequence, target_counter, target_key, assigned);
    RETURN assigned;
END;
$$;
REVOKE ALL ON FUNCTION platform.assign_counter(uuid, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.assign_counter(uuid, text, text) TO wamn_app;

CREATE OR REPLACE FUNCTION platform.set_counter(target_sequence uuid, target_counter text,
                                                target_key text, target_value bigint)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
BEGIN
    IF NOT EXISTS (SELECT FROM platform.number_sequence AS s
                    WHERE s.id = target_sequence
                      AND s.application = NULLIF(current_setting('app.application', true), '')) THEN
        RAISE EXCEPTION 'number sequence % is not a sequence of the claimed application', target_sequence
            USING ERRCODE = 'undefined_object';
    END IF;
    IF target_value IS NULL OR target_value < 1 THEN
        RAISE EXCEPTION 'counter % takes a value of 1 or more, not %', target_counter, target_value
            USING ERRCODE = 'invalid_parameter_value';
    END IF;
    INSERT INTO platform.number_counter AS c (sequence_id, counter, key_value, value)
    VALUES (target_sequence, target_counter, target_key, target_value)
    ON CONFLICT (sequence_id, counter, key_value)
    DO UPDATE SET value = EXCLUDED.value;
END;
$$;
REVOKE ALL ON FUNCTION platform.set_counter(uuid, text, text, bigint) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.set_counter(uuid, text, text, bigint) TO wamn_app;

DO $workload_acl$ DECLARE role_name text := 'wamn_administration'; BEGIN PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); ELSIF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolinherit OR rolreplication OR rolbypassrls)) THEN EXECUTE format('ALTER ROLE %I NOLOGIN PASSWORD NULL NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name AND rolsuper) THEN EXECUTE format('ALTER ROLE %I NOSUPERUSER', role_name); END IF; END IF; END $workload_acl$; REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, platform, "wamn_run" FROM "wamn_administration"; REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, platform, wamn_history, "wamn_run" FROM "wamn_administration"; REVOKE ALL PRIVILEGES ON SCHEMA catalog, platform, wamn_history, "wamn_run" FROM "wamn_administration"; GRANT USAGE ON SCHEMA catalog, platform, wamn_history TO "wamn_administration"; GRANT EXECUTE ON FUNCTION wamn_history.row_image(record) TO "wamn_administration"; GRANT SELECT ON TABLE catalog.releases TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."user" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."user_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."role" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."role_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."user_role" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."user_role_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."permission" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."permission_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."environment" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."environment_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."external_system" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."external_system_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."connection" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."connection_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."project_connection" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."project_connection_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."integration_allow" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."integration_allow_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."number_sequence" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."number_sequence_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."setting" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."setting_history" TO "wamn_administration"; GRANT EXECUTE ON FUNCTION "wamn_authority".tenant_key(text) TO "wamn_administration";
