-- The application account table is platform."user", singular
-- (docs/plan/platform-config.md; no table name is plural). This file renames
-- platform.users and its history table in an installed project database.
-- `user` is a reserved word, so SQL quotes the name. Each table keeps its
-- oid, rows, grants, triggers, policies and foreign keys. The history log
-- trigger writes to <table>_history by the table name, so the history table
-- is renamed with it. Each policy, index, sequence and constraint whose name
-- starts with users_ takes the name that app-schema.sql gives it on a fresh
-- install. No alias or view keeps the old name. upgrade-schema runs this
-- file once in one transaction and records it. A fresh install records it as
-- applied, because app-schema.sql already creates platform."user".
--
-- The rest is the output of
-- wamn_control_provision::sql::grant_administration_surface_sql("wamn_run"),
-- the latest rendering of the administration surface, which names
-- platform."user". A test in crates/control/provision/src/schema_migrations.rs
-- renders that function and compares the bytes. 0003, 0006, 0008, 0016, 0020,
-- 0024 and 0026 hold the earlier surfaces.

ALTER TABLE platform.users RENAME TO "user";
ALTER TABLE platform.users_history RENAME TO user_history;
ALTER SEQUENCE platform.users_history_position_seq RENAME TO user_history_position_seq;
ALTER POLICY users_tenant ON platform."user" RENAME TO user_tenant;
ALTER POLICY users_platform ON platform."user" RENAME TO user_platform;
ALTER POLICY users_history_tenant ON platform.user_history RENAME TO user_history_tenant;
ALTER POLICY users_history_platform ON platform.user_history RENAME TO user_history_platform;
ALTER INDEX platform.users_tkey RENAME TO user_tkey;
ALTER INDEX platform.users_history_tkey RENAME TO user_history_tkey;
DO $rename_constraints$ DECLARE target record; BEGIN
  FOR target IN
    SELECT c.conrelid::regclass AS relation, c.conname
      FROM pg_catalog.pg_constraint c
     WHERE c.conrelid IN ('platform."user"'::regclass, 'platform.user_history'::regclass)
       AND c.conname LIKE 'users\_%'
  LOOP
    EXECUTE format('ALTER TABLE %s RENAME CONSTRAINT %I TO %I',
      target.relation, target.conname, 'user' || substr(target.conname, 6));
  END LOOP;
END $rename_constraints$;

DO $workload_acl$ DECLARE role_name text := 'wamn_administration'; BEGIN PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); ELSIF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolinherit OR rolreplication OR rolbypassrls)) THEN EXECUTE format('ALTER ROLE %I NOLOGIN PASSWORD NULL NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name AND rolsuper) THEN EXECUTE format('ALTER ROLE %I NOSUPERUSER', role_name); END IF; END IF; END $workload_acl$; REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, platform, "wamn_run" FROM "wamn_administration"; REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, platform, wamn_history, "wamn_run" FROM "wamn_administration"; REVOKE ALL PRIVILEGES ON SCHEMA catalog, platform, wamn_history, "wamn_run" FROM "wamn_administration"; GRANT USAGE ON SCHEMA catalog, platform, wamn_history TO "wamn_administration"; GRANT EXECUTE ON FUNCTION wamn_history.row_image(record) TO "wamn_administration"; GRANT SELECT ON TABLE catalog.releases TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."user" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."user_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."roles" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."roles_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."user_roles" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."user_roles_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."permissions" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."permissions_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."environment" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."environment_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."external_system" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."external_system_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."connection" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."connection_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."project_connection" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."project_connection_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."integration_allow" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."integration_allow_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."number_sequence" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."number_sequence_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."setting" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."setting_history" TO "wamn_administration"; GRANT EXECUTE ON FUNCTION "wamn_authority".tenant_key(text) TO "wamn_administration";
