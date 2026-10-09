-- The creation saga and environment copy are removed (wamn-snz0.6, owner
-- ruling of 2026-10-09, docs/plan/platform-deploy.md section 12.1): creation
-- is the first apply of a document and copy is the same document under
-- another coordinate. The environment.create, environment.copy,
-- environment.resume and environment.abandon routes and wamn-ctl serve go,
-- so provisioning.saga_steps and provisioning.sagas go with them. The
-- control family loses USAGE on provisioning. This file carries the whole
-- rendered control surface, as 0015 did.
DROP TABLE provisioning.saga_steps;
DROP TABLE provisioning.sagas;
DO $control_surface$ BEGIN
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_control') THEN
    REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; REVOKE ALL PRIVILEGES ON SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; GRANT USAGE ON SCHEMA "catalog", "identity", "registry" TO "wamn_control"; GRANT SELECT ON TABLE "catalog"."package_artifacts" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_roles" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."password_logins" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."principals" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_env_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_roles" TO "wamn_control"; GRANT SELECT ON TABLE "registry"."project_envs" TO "wamn_control"; GRANT SELECT, INSERT ON TABLE "registry"."projects" TO "wamn_control"; GRANT UPDATE ("status") ON TABLE "registry"."project_envs" TO "wamn_control";
  END IF;
END $control_surface$;
