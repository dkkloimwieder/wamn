-- The platform owns schema platform in each project database
-- (docs/plan/platform-config.md, C1): this file renames app_system to
-- platform. The schema keeps its owner, its grants, its default privileges
-- and every object in it, which name the schema by its oid. No function
-- body, function search_path or stored row names app_system. upgrade-schema
-- runs this file once in one transaction and records it in
-- platform.schema_migrations, the record table that the rename moves. A
-- fresh install records it as applied, because app-schema.sql already
-- creates platform.
--
-- The rest is the output of
-- wamn_control_provision::sql::grant_administration_surface_sql("wamn_run"),
-- the latest rendering of the administration surface, which names platform.
-- A test in crates/control/provision/src/schema_migrations.rs renders that
-- function and compares the bytes. 0003, 0006, 0008, 0016 and 0020 hold the
-- earlier surfaces.

ALTER SCHEMA app_system RENAME TO platform;

DO $workload_acl$ DECLARE role_name text := 'wamn_administration'; BEGIN PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); ELSIF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolinherit OR rolreplication OR rolbypassrls)) THEN EXECUTE format('ALTER ROLE %I NOLOGIN PASSWORD NULL NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name AND rolsuper) THEN EXECUTE format('ALTER ROLE %I NOSUPERUSER', role_name); END IF; END IF; END $workload_acl$; REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, platform, "wamn_run" FROM "wamn_administration"; REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, platform, wamn_history, "wamn_run" FROM "wamn_administration"; REVOKE ALL PRIVILEGES ON SCHEMA catalog, platform, wamn_history, "wamn_run" FROM "wamn_administration"; GRANT USAGE ON SCHEMA catalog, platform, wamn_history TO "wamn_administration"; GRANT EXECUTE ON FUNCTION wamn_history.row_image(record) TO "wamn_administration"; GRANT SELECT ON TABLE catalog.releases TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."users" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."users_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."roles" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."roles_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."user_roles" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."user_roles_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."permissions" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."permissions_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."environment" TO "wamn_administration"; GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."environment_history" TO "wamn_administration"; GRANT EXECUTE ON FUNCTION "wamn_authority".tenant_key(text) TO "wamn_administration";
