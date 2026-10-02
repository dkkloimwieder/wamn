-- One row per package artifact that `push-package` pushed (wamn-zua8.3,
-- docs/plan/platform-ui.md section 6 item 10). A package artifact belongs to no
-- tenant, so its policy admits every row; the catalog keeps its RLS floor. The
-- row is an immutable fact, like a deployment attestation.
CREATE TABLE catalog.package_artifacts (
    package_id    text        NOT NULL CHECK (package_id <> ''),
    version       text        NOT NULL CHECK (version <> ''),
    digest        text        NOT NULL CHECK (digest ~ '^sha256:[0-9a-f]{64}$'),
    source_commit text        CHECK (source_commit IS NULL OR source_commit <> ''),
    attested_at   timestamptz NOT NULL DEFAULT clock_timestamp(),
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
