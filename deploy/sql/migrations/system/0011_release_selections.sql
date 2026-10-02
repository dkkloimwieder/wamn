-- Select reuses a qualification (wamn-zua8.3, docs/plan/platform-ui.md section
-- 6 item 10). catalog.qualifications holds one row per passing qualification
-- that a select read from a file. A qualification proves bytes, not names: the
-- package set is the (package_id, version, component_digest) triples of the
-- release, and the image digests are those of the host, gates and identity
-- images. It belongs to no tenant, so its policy admits every row.
-- catalog.release_selections records every select, with the qualification it
-- used. Both are immutable facts.
CREATE TABLE catalog.qualifications (
    qualification_sha256 text        NOT NULL
        CHECK (qualification_sha256 ~ '^sha256:[0-9a-f]{64}$'),
    package_set          jsonb       NOT NULL CHECK (jsonb_typeof(package_set) = 'array'),
    image_digests        jsonb       NOT NULL CHECK (jsonb_typeof(image_digests) = 'object'),
    recorded_at          timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT qualifications_pkey PRIMARY KEY (qualification_sha256)
);
ALTER TABLE catalog.qualifications ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.qualifications FORCE ROW LEVEL SECURITY;
CREATE POLICY qualifications_all ON catalog.qualifications
    USING (true) WITH CHECK (true);
CREATE TRIGGER qualifications_immutable
    BEFORE UPDATE OR DELETE ON catalog.qualifications
    FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
REVOKE ALL ON TABLE catalog.qualifications FROM PUBLIC;

CREATE TABLE catalog.release_selections (
    tenant_id            text        NOT NULL CHECK (tenant_id <> ''),
    environment          text        NOT NULL CHECK (environment <> ''),
    effective_release_id int         NOT NULL CHECK (effective_release_id > 0),
    qualification_sha256 text        NOT NULL,
    selected_at          timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT release_selections_pkey
        PRIMARY KEY (tenant_id, environment, selected_at),
    CONSTRAINT release_selections_release_fkey
        FOREIGN KEY (tenant_id, effective_release_id, environment)
        REFERENCES catalog.effective_releases
            (tenant_id, effective_release_id, environment),
    CONSTRAINT release_selections_qualification_fkey
        FOREIGN KEY (qualification_sha256)
        REFERENCES catalog.qualifications (qualification_sha256)
);
ALTER TABLE catalog.release_selections ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.release_selections FORCE ROW LEVEL SECURITY;
CREATE POLICY release_selections_tenant ON catalog.release_selections
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
CREATE TRIGGER release_selections_immutable
    BEFORE UPDATE OR DELETE ON catalog.release_selections
    FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
REVOKE ALL ON TABLE catalog.release_selections FROM PUBLIC;
