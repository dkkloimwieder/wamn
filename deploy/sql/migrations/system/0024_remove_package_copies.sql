-- Remove the system copies of the package facts (wamn-unffv.1,
-- docs/plan/platform-deploy.md §17.2 Epic 9, R2). The project database holds
-- catalog.packages and catalog.package_migrations, and nothing reads or writes
-- the system copies. effective_release_packages had no writer, and it only
-- tied a release to the system packages and sealed their migrations, so it
-- goes too. catalog.effective_releases stays. Dropping the tables also drops
-- their policies and triggers, and the two seal functions drop after them.
--
-- catalog.package_artifacts goes too (wamn-vavs4.11, R11): the OCI registry
-- is the one owner of package facts, push-package stores no verification, and
-- every reader pulls a package by digest or tag from the registry. The drop
-- removes the SELECT of the control family and of the control author. The
-- package.list route that read the table is gone, so the control family
-- loses USAGE on catalog, and this file carries the whole rendered control
-- surface, as 0020 did. A fresh install records this file as applied,
-- because control-portable-store.sql no longer creates the tables.

DROP TABLE catalog.effective_release_packages;
DROP TABLE catalog.package_migrations;
DROP TABLE catalog.packages;
DROP FUNCTION catalog.lock_package_coordinate_for_release_membership();
DROP FUNCTION catalog.reject_package_migration_after_release_membership();
DROP TABLE catalog.package_artifacts;
DO $control_surface$ BEGIN
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_control') THEN
    REVOKE ALL PRIVILEGES ON SCHEMA "catalog" FROM "wamn_control";
    REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; REVOKE ALL PRIVILEGES ON SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; GRANT USAGE ON SCHEMA "identity", "registry" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_roles" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."password_logins" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."principals" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_env_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_roles" TO "wamn_control"; GRANT SELECT ON TABLE "registry"."project_envs" TO "wamn_control"; GRANT SELECT, INSERT ON TABLE "registry"."projects" TO "wamn_control"; GRANT UPDATE ("status") ON TABLE "registry"."project_envs" TO "wamn_control";
  END IF;
END $control_surface$;
