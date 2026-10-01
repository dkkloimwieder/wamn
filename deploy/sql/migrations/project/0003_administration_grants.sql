-- The administration family surface of docs/plan/platform-ui.md §2.1 in an
-- installed project-environment database (wamn-a40n.2).
-- upgrade-schema runs this file once in one transaction and records it. A
-- fresh install records it as applied, because provision-project-env applies
-- the same surface when it prepares the administration credential.
--
-- The grant goes to the family role wamn_administration only, not to
-- wamn_platform, which every platform family inherits (owner correction of
-- 2026-09-30). The role reads and writes users, roles, user_roles and
-- permissions, appends the entries of their history tables, and holds nothing
-- on the catalog or the run plane. The statements are those of
-- wamn_control_provision::sql::grant_administration_surface_sql("wamn_run").

DO $workload_acl$ DECLARE role_name text := 'wamn_administration'; BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN
    EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE
      NOINHERIT NOREPLICATION NOBYPASSRLS', role_name);
  ELSIF EXISTS (SELECT FROM pg_catalog.pg_authid WHERE rolname = role_name
                AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole
                     OR rolinherit OR rolreplication OR rolbypassrls
                     OR rolpassword IS NOT NULL)) THEN
    EXECUTE format('ALTER ROLE %I NOLOGIN PASSWORD NULL NOSUPERUSER NOCREATEDB
      NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name);
  END IF;
END $workload_acl$;

REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, app_system, wamn_run FROM wamn_administration;
REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, app_system, wamn_history, wamn_run
    FROM wamn_administration;
REVOKE ALL PRIVILEGES ON SCHEMA catalog, app_system, wamn_history, wamn_run FROM wamn_administration;
GRANT USAGE ON SCHEMA app_system, wamn_history TO wamn_administration;
GRANT EXECUTE ON FUNCTION wamn_history.row_image(record) TO wamn_administration;

GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE app_system.users TO wamn_administration;
GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at,
              transaction_id, before, after)
    ON TABLE app_system.users_history TO wamn_administration;
GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE app_system.roles TO wamn_administration;
GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at,
              transaction_id, before, after)
    ON TABLE app_system.roles_history TO wamn_administration;
GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE app_system.user_roles TO wamn_administration;
GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at,
              transaction_id, before, after)
    ON TABLE app_system.user_roles_history TO wamn_administration;
GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE app_system.permissions TO wamn_administration;
GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at,
              transaction_id, before, after)
    ON TABLE app_system.permissions_history TO wamn_administration;

GRANT EXECUTE ON FUNCTION wamn_authority.tenant_key(text) TO wamn_administration;
