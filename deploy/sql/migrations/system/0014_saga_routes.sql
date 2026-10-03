-- Saga resume and abandon (wamn-zua8.4, docs/plan/platform-ui.md section 5.2,
-- owner ruling of 2026-10-03). The environment.resume and environment.abandon
-- routes set the status of a create-environment saga, so the control family
-- gains UPDATE on the status, last_error and updated_at columns of
-- provisioning.sagas. It never writes provisioning.saga_steps. This file
-- carries the whole rendered control surface, as 0013 did.
DO $control_surface$ BEGIN
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_control') THEN
    REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; REVOKE ALL PRIVILEGES ON SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; GRANT USAGE ON SCHEMA "identity", "provisioning", "registry" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_roles" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."password_logins" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."principals" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_env_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_roles" TO "wamn_control"; GRANT SELECT, INSERT ON TABLE "provisioning"."saga_steps" TO "wamn_control"; GRANT SELECT, INSERT ON TABLE "provisioning"."sagas" TO "wamn_control"; GRANT SELECT ON TABLE "registry"."project_envs" TO "wamn_control"; GRANT SELECT, INSERT ON TABLE "registry"."projects" TO "wamn_control"; GRANT UPDATE ("status", "last_error", "updated_at") ON TABLE "provisioning"."sagas" TO "wamn_control"; GRANT UPDATE ("status") ON TABLE "registry"."project_envs" TO "wamn_control";
  END IF;
END $control_surface$;
