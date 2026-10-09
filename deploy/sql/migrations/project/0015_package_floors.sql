-- The recorded package floors, and the removal of bindings after the drain
-- (docs/plan/platform-deploy.md R13, R16, R22 (3), wamn-snz0.4).
-- catalog.package_floors records the version `env apply` contracted each
-- installed package to at step 9. A connection binding still never changes,
-- but `env apply` deletes the bindings of a release that left the live set,
-- so its immutability trigger covers UPDATE only.
-- upgrade-schema runs this file once in one transaction and records it. A
-- fresh install records it as applied, because catalog-schema.sql already
-- holds the new shape.
CREATE TABLE catalog.package_floors (
    tenant_id   text        NOT NULL CHECK (tenant_id <> ''),
    package_id  text        NOT NULL,
    version     text        NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT package_floors_pkey PRIMARY KEY (tenant_id, package_id),
    CONSTRAINT package_floors_package_fkey
        FOREIGN KEY (tenant_id, package_id, version)
        REFERENCES catalog.packages (tenant_id, package_id, package_version)
);
ALTER TABLE catalog.package_floors ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.package_floors FORCE ROW LEVEL SECURITY;
CREATE POLICY package_floors_tenant ON catalog.package_floors TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY package_floors_platform ON catalog.package_floors
    AS PERMISSIVE FOR ALL TO wamn_platform USING (true) WITH CHECK (true);
CREATE INDEX package_floors_tkey
    ON catalog.package_floors ((wamn_authority.tenant_key(tenant_id)));
REVOKE ALL ON catalog.package_floors FROM PUBLIC;

DROP TRIGGER connection_bindings_immutable ON catalog.connection_bindings;
CREATE TRIGGER connection_bindings_immutable
    BEFORE UPDATE ON catalog.connection_bindings
    FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
