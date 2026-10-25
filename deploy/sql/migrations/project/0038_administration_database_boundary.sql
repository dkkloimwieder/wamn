-- C3.1: the project database separates administration rows by environment.
-- Keep retained history images. Re-key history entries to the new row keys.
-- Duplicate former tenant keys refuse the migration rather than lose rows.
DO $role_foreign_keys$ DECLARE target record; BEGIN
  FOR target IN
    SELECT conrelid::regclass AS relation, conname FROM pg_catalog.pg_constraint
     WHERE contype = 'f' AND confrelid = 'platform.role'::regclass
       AND conrelid IN ('platform.user_role'::regclass, 'platform.permission'::regclass)
  LOOP
    EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I', target.relation, target.conname);
  END LOOP;
END $role_foreign_keys$;

DO $administration_boundary$ DECLARE relation text; target text; BEGIN
  FOREACH relation IN ARRAY ARRAY['role', 'user_role', 'permission', 'api_key', 'environment'] LOOP
    FOREACH target IN ARRAY ARRAY[relation, relation || '_history'] LOOP
      EXECUTE format('DROP POLICY %I ON platform.%I', target || '_tenant', target);
      EXECUTE format('DROP POLICY %I ON platform.%I', target || '_platform', target);
      EXECUTE format('DROP INDEX platform.%I', target || '_tkey');
      EXECUTE format('ALTER TABLE platform.%I NO FORCE ROW LEVEL SECURITY', target);
      EXECUTE format('ALTER TABLE platform.%I DISABLE ROW LEVEL SECURITY', target);
      EXECUTE format('ALTER TABLE platform.%I DROP COLUMN tenant_id', target);
    END LOOP;
    IF relation = 'environment' THEN
      UPDATE platform.environment_history SET row_key = '{"id":true}'::jsonb;
    ELSE
      EXECUTE format('UPDATE platform.%I SET row_key = row_key - ''tenant_id''', relation || '_history');
    END IF;
  END LOOP;
END $administration_boundary$;

ALTER TABLE platform.role ADD PRIMARY KEY (name);
ALTER TABLE platform.user_role ADD PRIMARY KEY (user_id, role_name);
ALTER TABLE platform.user_role ADD FOREIGN KEY (role_name)
    REFERENCES platform.role (name) ON DELETE CASCADE;
ALTER TABLE platform.permission ADD PRIMARY KEY (role_name, permission);
ALTER TABLE platform.permission ADD FOREIGN KEY (role_name)
    REFERENCES platform.role (name) ON DELETE CASCADE;
ALTER TABLE platform.api_key ADD PRIMARY KEY (id);
ALTER TABLE platform.api_key ADD UNIQUE (key_hash);
ALTER TABLE platform.environment ADD COLUMN id boolean NOT NULL DEFAULT true CHECK (id);
ALTER TABLE platform.environment ADD PRIMARY KEY (id);

-- Existing history INSERT grants survive the column drop. The reader loses
-- only the removed column, and a fresh grant has the same four-column shape.


DO $workload_acl$ DECLARE role_name text := 'wamn_administration'; BEGIN PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); ELSIF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolinherit OR rolreplication OR rolbypassrls)) THEN EXECUTE format('ALTER ROLE %I NOLOGIN PASSWORD NULL NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name AND rolsuper) THEN EXECUTE format('ALTER ROLE %I NOSUPERUSER', role_name); END IF; END IF; END $workload_acl$; REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, platform, "wamn_run" FROM "wamn_administration"; REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, platform, wamn_history, "wamn_run" FROM "wamn_administration"; REVOKE ALL PRIVILEGES ON SCHEMA catalog, platform, wamn_history, "wamn_run" FROM "wamn_administration"; REVOKE EXECUTE ON FUNCTION wamn_authority.tenant_key(text) FROM "wamn_administration"; GRANT USAGE ON SCHEMA catalog, platform, wamn_history TO "wamn_administration"; GRANT EXECUTE ON FUNCTION wamn_history.row_image(record) TO "wamn_administration"; GRANT SELECT ON TABLE catalog.releases TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."user" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."user_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."role" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."role_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."user_role" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."user_role_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."permission" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."permission_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."environment" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."environment_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."external_system" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."external_system_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."connection" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."connection_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."project_connection" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."project_connection_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."integration_allow" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."integration_allow_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."number_sequence" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."number_sequence_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."setting" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."setting_history" TO "wamn_administration";
