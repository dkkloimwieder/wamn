-- No table name is plural (docs/plan/platform-config.md). This file renames
-- platform.roles, platform.user_roles, platform.permissions and
-- platform.api_keys to platform.role, platform.user_role, platform.permission
-- and platform.api_key in an installed project database, with their history
-- tables. `role` is a non-reserved keyword, so SQL does not quote the name.
-- Each table keeps its oid, rows, grants, triggers, policies and foreign keys.
-- The history log trigger writes to <table>_history by the table name, so
-- each history table is renamed with its table. Each policy, index, sequence
-- and constraint whose name starts with the plural table name takes the name
-- that app-schema.sql gives it on a fresh install. No alias or view keeps the
-- old names. upgrade-schema runs this file once in one transaction and
-- records it. A fresh install records it as applied, because app-schema.sql
-- already creates the singular tables.
--
-- The rest is the output of
-- wamn_control_provision::sql::grant_administration_surface_sql("wamn_run"),
-- the latest rendering of the administration surface, which names the
-- singular tables. A test in crates/control/provision/src/schema_migrations.rs
-- renders that function and compares the bytes. 0003, 0006, 0008, 0016, 0020,
-- 0024, 0026 and 0027 hold the earlier surfaces.

ALTER TABLE platform.roles RENAME TO role;
ALTER TABLE platform.roles_history RENAME TO role_history;
ALTER TABLE platform.user_roles RENAME TO user_role;
ALTER TABLE platform.user_roles_history RENAME TO user_role_history;
ALTER TABLE platform.permissions RENAME TO permission;
ALTER TABLE platform.permissions_history RENAME TO permission_history;
ALTER TABLE platform.api_keys RENAME TO api_key;
ALTER TABLE platform.api_keys_history RENAME TO api_key_history;
ALTER SEQUENCE platform.roles_history_position_seq RENAME TO role_history_position_seq;
ALTER SEQUENCE platform.user_roles_history_position_seq RENAME TO user_role_history_position_seq;
ALTER SEQUENCE platform.permissions_history_position_seq RENAME TO permission_history_position_seq;
ALTER SEQUENCE platform.api_keys_history_position_seq RENAME TO api_key_history_position_seq;
ALTER POLICY roles_tenant ON platform.role RENAME TO role_tenant;
ALTER POLICY roles_platform ON platform.role RENAME TO role_platform;
ALTER POLICY roles_history_tenant ON platform.role_history RENAME TO role_history_tenant;
ALTER POLICY roles_history_platform ON platform.role_history RENAME TO role_history_platform;
ALTER POLICY user_roles_tenant ON platform.user_role RENAME TO user_role_tenant;
ALTER POLICY user_roles_platform ON platform.user_role RENAME TO user_role_platform;
ALTER POLICY user_roles_history_tenant ON platform.user_role_history RENAME TO user_role_history_tenant;
ALTER POLICY user_roles_history_platform ON platform.user_role_history RENAME TO user_role_history_platform;
ALTER POLICY permissions_tenant ON platform.permission RENAME TO permission_tenant;
ALTER POLICY permissions_platform ON platform.permission RENAME TO permission_platform;
ALTER POLICY permissions_history_tenant ON platform.permission_history RENAME TO permission_history_tenant;
ALTER POLICY permissions_history_platform ON platform.permission_history RENAME TO permission_history_platform;
ALTER POLICY api_keys_tenant ON platform.api_key RENAME TO api_key_tenant;
ALTER POLICY api_keys_platform ON platform.api_key RENAME TO api_key_platform;
ALTER POLICY api_keys_history_tenant ON platform.api_key_history RENAME TO api_key_history_tenant;
ALTER POLICY api_keys_history_platform ON platform.api_key_history RENAME TO api_key_history_platform;
ALTER INDEX platform.roles_tkey RENAME TO role_tkey;
ALTER INDEX platform.roles_history_tkey RENAME TO role_history_tkey;
ALTER INDEX platform.user_roles_tkey RENAME TO user_role_tkey;
ALTER INDEX platform.user_roles_history_tkey RENAME TO user_role_history_tkey;
ALTER INDEX platform.permissions_tkey RENAME TO permission_tkey;
ALTER INDEX platform.permissions_history_tkey RENAME TO permission_history_tkey;
ALTER INDEX platform.api_keys_tkey RENAME TO api_key_tkey;
ALTER INDEX platform.api_keys_history_tkey RENAME TO api_key_history_tkey;
DO $rename_constraints$ DECLARE target record; BEGIN
  FOR target IN
    SELECT c.conrelid::regclass AS relation, c.conname, names.plural, names.singular
      FROM pg_catalog.pg_constraint c
      JOIN (VALUES ('platform.role'::regclass, 'roles', 'role'),
                   ('platform.role_history'::regclass, 'roles', 'role'),
                   ('platform.user_role'::regclass, 'user_roles', 'user_role'),
                   ('platform.user_role_history'::regclass, 'user_roles', 'user_role'),
                   ('platform.permission'::regclass, 'permissions', 'permission'),
                   ('platform.permission_history'::regclass, 'permissions', 'permission'),
                   ('platform.api_key'::regclass, 'api_keys', 'api_key'),
                   ('platform.api_key_history'::regclass, 'api_keys', 'api_key'))
        AS names (relation, plural, singular) ON names.relation = c.conrelid
     WHERE starts_with(c.conname, names.plural || '_')
  LOOP
    EXECUTE format('ALTER TABLE %s RENAME CONSTRAINT %I TO %I',
      target.relation, target.conname,
      target.singular || substr(target.conname, length(target.plural) + 1));
  END LOOP;
END $rename_constraints$;

DO $workload_acl$ DECLARE role_name text := 'wamn_administration'; BEGIN PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); ELSIF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolinherit OR rolreplication OR rolbypassrls)) THEN EXECUTE format('ALTER ROLE %I NOLOGIN PASSWORD NULL NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name AND rolsuper) THEN EXECUTE format('ALTER ROLE %I NOSUPERUSER', role_name); END IF; END IF; END $workload_acl$; REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, platform, "wamn_run" FROM "wamn_administration"; REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, platform, wamn_history, "wamn_run" FROM "wamn_administration"; REVOKE ALL PRIVILEGES ON SCHEMA catalog, platform, wamn_history, "wamn_run" FROM "wamn_administration"; GRANT USAGE ON SCHEMA catalog, platform, wamn_history TO "wamn_administration"; GRANT EXECUTE ON FUNCTION wamn_history.row_image(record) TO "wamn_administration"; GRANT SELECT ON TABLE catalog.releases TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."user" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."user_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."role" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."role_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."user_role" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."user_role_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."permission" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."permission_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."environment" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."environment_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."external_system" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."external_system_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."connection" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."connection_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."project_connection" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."project_connection_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."integration_allow" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."integration_allow_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."number_sequence" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."number_sequence_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."setting" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."setting_history" TO "wamn_administration"; GRANT EXECUTE ON FUNCTION "wamn_authority".tenant_key(text) TO "wamn_administration";
