-- The status of each project environment (wamn-zua8.2, docs/plan/platform-ui.md
-- §5.4). Identity offers no audience of an inactive environment, and its host
-- serves none of its routes. Every existing environment is active.
-- The control family writes the status column and no other column of the
-- table. This file carries the whole rendered control surface, as 0007 did.
-- An installed database whose issuer or control family was never prepared
-- has no such role, and its first prepare grants the surface, so each grant
-- runs only when its role exists.
ALTER TABLE registry.project_envs
    ADD COLUMN status text NOT NULL DEFAULT 'active',
    ADD CONSTRAINT project_envs_status_check CHECK (status IN ('active', 'inactive'));

DO $environment_status$ BEGIN
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_identity_issuer') THEN
    GRANT SELECT (status) ON TABLE registry.project_envs TO wamn_identity_issuer;
  END IF;
END $environment_status$;

DO $control_surface$ BEGIN
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_control') THEN
    REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; REVOKE ALL PRIVILEGES ON SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; GRANT USAGE ON SCHEMA "identity", "registry" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_roles" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."password_logins" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."principals" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_env_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_roles" TO "wamn_control"; GRANT SELECT ON TABLE "registry"."project_envs" TO "wamn_control"; GRANT SELECT ON TABLE "registry"."projects" TO "wamn_control"; GRANT UPDATE ("status") ON TABLE "registry"."project_envs" TO "wamn_control";
  END IF;
END $control_surface$;
