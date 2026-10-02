-- The status of a project environment (wamn-zua8.2, docs/plan/platform-ui.md
-- §5.4): app_system.environment and its history table, as
-- deploy/sql/app-schema.sql creates them on a fresh install. An installed
-- database has no row, which means active. upgrade-schema runs this file once
-- in one transaction and records it. Project migration 0008 grants the
-- administration family its write.

CREATE TABLE app_system.environment (
    tenant_id  text NOT NULL CHECK (tenant_id <> ''),
    status     text NOT NULL CHECK (status IN ('active', 'inactive')),
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (tenant_id)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON app_system.environment
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');
ALTER TABLE app_system.environment ENABLE ROW LEVEL SECURITY;
ALTER TABLE app_system.environment FORCE ROW LEVEL SECURITY;
CREATE POLICY environment_tenant ON app_system.environment
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY environment_platform ON app_system.environment
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX environment_tkey
    ON app_system.environment ((wamn_authority.tenant_key(tenant_id)));
GRANT SELECT ON app_system.environment TO wamn_app;

SELECT wamn_history.create_history_table('app_system', 'environment', true);

ALTER TABLE app_system.environment_history ENABLE ROW LEVEL SECURITY;
ALTER TABLE app_system.environment_history FORCE ROW LEVEL SECURITY;
CREATE POLICY environment_history_tenant ON app_system.environment_history
    TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY environment_history_platform ON app_system.environment_history
    AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true)
    WITH CHECK (true);
CREATE INDEX environment_history_tkey
    ON app_system.environment_history ((wamn_authority.tenant_key(tenant_id)));
CREATE TRIGGER wamn_record_history_log
    AFTER INSERT OR UPDATE OR DELETE ON app_system.environment
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.log_row_change('unlimited');
