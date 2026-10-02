-- Accepted package-upgrade evidence is immutable platform state (wamn-xvu5.1).
-- apply-package writes it in the transaction that installs the candidate.

CREATE TABLE catalog.package_upgrade_qualifications (
    tenant_id                   text        NOT NULL CHECK (tenant_id <> ''),
    package_id                  text        NOT NULL CHECK (package_id <> ''),
    candidate_package_version   text        NOT NULL CHECK (candidate_package_version <> ''),
    canonical_bytes             bytea       NOT NULL CHECK (octet_length(canonical_bytes) > 0),
    result_sha256               text        NOT NULL CHECK (result_sha256 ~ '^sha256:[0-9a-f]{64}$'),
    predecessor_release_id      int         NOT NULL CHECK (predecessor_release_id > 0),
    predecessor_manifest_digest text        NOT NULL
        CHECK (predecessor_manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    recorded_at                 timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT package_upgrade_qualifications_pkey
        PRIMARY KEY (tenant_id, package_id, candidate_package_version),
    CONSTRAINT package_upgrade_qualifications_package_fkey
        FOREIGN KEY (tenant_id, package_id, candidate_package_version)
        REFERENCES catalog.packages (tenant_id, package_id, package_version),
    CONSTRAINT package_upgrade_qualifications_predecessor_fkey
        FOREIGN KEY (tenant_id, predecessor_release_id)
        REFERENCES catalog.release_manifest_snapshots (tenant_id, effective_release_id),
    CONSTRAINT package_upgrade_qualifications_exact_hash
        CHECK (result_sha256 = 'sha256:' || encode(sha256(canonical_bytes), 'hex'))
);

ALTER TABLE catalog.package_upgrade_qualifications ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.package_upgrade_qualifications FORCE ROW LEVEL SECURITY;
CREATE POLICY package_upgrade_qualifications_tenant
    ON catalog.package_upgrade_qualifications TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY package_upgrade_qualifications_platform
    ON catalog.package_upgrade_qualifications AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true) WITH CHECK (true);
CREATE INDEX package_upgrade_qualifications_tkey
    ON catalog.package_upgrade_qualifications ((wamn_authority.tenant_key(tenant_id)));
CREATE TRIGGER package_upgrade_qualifications_immutable
    BEFORE UPDATE OR DELETE ON catalog.package_upgrade_qualifications
    FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
REVOKE ALL ON catalog.package_upgrade_qualifications FROM PUBLIC;
