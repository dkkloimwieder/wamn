-- One package artifact holds the package and the descriptors of its
-- components (wamn-vavs4.1, docs/plan/platform-deploy.md section 7.2, contract
-- A). catalog.package_artifacts records the digest of its image manifest and
-- verified_at, which push-package writes once, after every check and push
-- succeeds. The old rows are layer digests of the single-layer artifact, which
-- no reader accepts, so the table is dropped and created again. The drop
-- removes the control family's SELECT, so this file carries the whole
-- rendered control surface, as 0020 did.
DROP TABLE catalog.package_artifacts;
CREATE TABLE catalog.package_artifacts (
    package_id    text        NOT NULL CHECK (package_id <> ''),
    version       text        NOT NULL CHECK (version <> ''),
    digest        text        NOT NULL CHECK (digest ~ '^sha256:[0-9a-f]{64}$'),
    source_commit text        CHECK (source_commit IS NULL OR source_commit <> ''),
    verified_at   timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT package_artifacts_pkey PRIMARY KEY (package_id, version)
);
ALTER TABLE catalog.package_artifacts ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.package_artifacts FORCE ROW LEVEL SECURITY;
CREATE POLICY package_artifacts_all ON catalog.package_artifacts
    USING (true) WITH CHECK (true);
CREATE TRIGGER package_artifacts_immutable
    BEFORE UPDATE OR DELETE ON catalog.package_artifacts
    FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
REVOKE ALL ON TABLE catalog.package_artifacts FROM PUBLIC;
DO $control_surface$ BEGIN
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_control') THEN
    REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; REVOKE ALL PRIVILEGES ON SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; GRANT USAGE ON SCHEMA "catalog", "identity", "registry" TO "wamn_control"; GRANT SELECT ON TABLE "catalog"."package_artifacts" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_roles" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."password_logins" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."principals" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_env_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_roles" TO "wamn_control"; GRANT SELECT ON TABLE "registry"."project_envs" TO "wamn_control"; GRANT SELECT, INSERT ON TABLE "registry"."projects" TO "wamn_control"; GRANT UPDATE ("status") ON TABLE "registry"."project_envs" TO "wamn_control";
  END IF;
END $control_surface$;
