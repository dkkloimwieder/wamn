-- Stage progress survives committed batches before the package version advances.
CREATE TABLE catalog.package_upgrade_stages (
    tenant_id            text NOT NULL CHECK (tenant_id <> ''),
    package_id           text NOT NULL CHECK (package_id <> ''),
    package_version      text NOT NULL CHECK (package_version <> ''),
    predecessor_version  text NOT NULL CHECK (predecessor_version <> '' AND predecessor_version <> package_version),
    manifest_sha256      text NOT NULL CHECK (manifest_sha256 ~ '^sha256:[0-9a-f]{64}$'),
    qualification_sha256 text NOT NULL CHECK (qualification_sha256 ~ '^sha256:[0-9a-f]{64}$'),
    status               text NOT NULL CHECK (status IN ('in_progress', 'abandoned', 'completed')),
    cursor               jsonb NOT NULL,
    completed_batches    bigint NOT NULL DEFAULT 0 CHECK (completed_batches >= 0),
    CONSTRAINT package_upgrade_stages_pkey PRIMARY KEY (tenant_id, package_id, package_version)
);

CREATE FUNCTION catalog.guard_package_upgrade_stage_change()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-upgrade-stage-cannot-be-deleted';
    END IF;
    IF ROW(NEW.tenant_id, NEW.package_id, NEW.package_version, NEW.predecessor_version,
           NEW.manifest_sha256, NEW.qualification_sha256)
       IS DISTINCT FROM
       ROW(OLD.tenant_id, OLD.package_id, OLD.package_version, OLD.predecessor_version,
           OLD.manifest_sha256, OLD.qualification_sha256) THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-upgrade-stage-identity-is-immutable';
    END IF;
    IF OLD.status <> 'in_progress' THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-upgrade-stage-is-terminal';
    END IF;
    IF NEW.completed_batches < OLD.completed_batches
       OR NEW.completed_batches > OLD.completed_batches + 1 THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-upgrade-stage-invalid-batch-count';
    END IF;
    IF NEW.completed_batches = OLD.completed_batches AND NEW.cursor IS DISTINCT FROM OLD.cursor THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-upgrade-stage-cursor-requires-batch';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER package_upgrade_stages_guard
    BEFORE UPDATE OR DELETE ON catalog.package_upgrade_stages
    FOR EACH ROW EXECUTE FUNCTION catalog.guard_package_upgrade_stage_change();
REVOKE ALL ON FUNCTION catalog.guard_package_upgrade_stage_change() FROM PUBLIC;
REVOKE ALL ON catalog.package_upgrade_stages FROM PUBLIC, wamn_app;

ALTER TABLE catalog.package_upgrade_stages ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.package_upgrade_stages FORCE ROW LEVEL SECURITY;
CREATE POLICY package_upgrade_stages_tenant
    ON catalog.package_upgrade_stages TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY package_upgrade_stages_platform
    ON catalog.package_upgrade_stages AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true) WITH CHECK (true);
CREATE INDEX package_upgrade_stages_tkey
    ON catalog.package_upgrade_stages ((wamn_authority.tenant_key(tenant_id)));

-- Synchronization definitions keep their package owner after contract removes them.
ALTER TABLE catalog.package_definition_owners
    DROP CONSTRAINT package_definition_owners_definition_type_check;
ALTER TABLE catalog.package_definition_owners
    ADD CONSTRAINT package_definition_owners_definition_type_check
    CHECK (definition_type IN ('relation', 'field', 'constraint', 'synchronization_function', 'synchronization_trigger'));
CREATE UNIQUE INDEX package_definition_owners_synchronization_function
    ON catalog.package_definition_owners (tenant_id, schema_name, definition_name)
    WHERE definition_type = 'synchronization_function';
