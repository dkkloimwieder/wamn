-- catalog.workflows (docs/plan/workflows.md R8, epic 1) in an installed
-- project database. The table, its index, its triggers, its tenant floor and
-- its grant are the ones deploy/sql/catalog-schema.sql creates. upgrade-schema
-- runs this file once in one transaction and records it. A fresh install
-- records it as applied, because catalog-schema.sql already holds the table.

CREATE TABLE catalog.workflows (
    tenant_id      text        NOT NULL CHECK (tenant_id <> ''),
    environment    text        NOT NULL CHECK (environment <> ''),
    application    text        NOT NULL CHECK (application <> ''),
    workflow_id    text        NOT NULL CHECK (workflow_id <> ''),
    version        int         NOT NULL CHECK (version > 0),
    graph_json     jsonb       NOT NULL,
    wiring_hash    text        NOT NULL CHECK (wiring_hash ~ '^sha256:[0-9a-f]{64}$'),
    trigger        text        NOT NULL CHECK (trigger = 'manual'),
    execution_type text        NOT NULL CHECK (execution_type IN ('role', 'invoker')),
    execution_role text        CHECK (execution_role <> ''),
    enabled        boolean     NOT NULL DEFAULT false,
    created_by     uuid        NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT workflows_pkey
        PRIMARY KEY (tenant_id, environment, application, workflow_id, version),
    CONSTRAINT workflows_execution_identity_check
        CHECK ((execution_type = 'role') = (execution_role IS NOT NULL))
);
CREATE UNIQUE INDEX workflows_one_enabled
    ON catalog.workflows (tenant_id, environment, application, workflow_id)
    WHERE enabled;

CREATE FUNCTION catalog.guard_workflow_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF ROW(NEW.tenant_id, NEW.environment, NEW.application, NEW.workflow_id,
           NEW.version, NEW.graph_json, NEW.wiring_hash, NEW.trigger,
           NEW.execution_type, NEW.execution_role, NEW.created_by, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.tenant_id, OLD.environment, OLD.application, OLD.workflow_id,
           OLD.version, OLD.graph_json, OLD.wiring_hash, OLD.trigger,
           OLD.execution_type, OLD.execution_role, OLD.created_by, OLD.created_at) THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'workflow-version-immutable';
    END IF;
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION catalog.guard_workflow_update() FROM PUBLIC;
CREATE TRIGGER workflows_controlled_update
    BEFORE UPDATE ON catalog.workflows
    FOR EACH ROW EXECUTE FUNCTION catalog.guard_workflow_update();
CREATE TRIGGER workflows_delete_immutable
    BEFORE DELETE ON catalog.workflows
    FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();

ALTER TABLE catalog.workflows ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.workflows FORCE ROW LEVEL SECURITY;
CREATE POLICY workflows_tenant ON catalog.workflows TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY workflows_platform ON catalog.workflows AS PERMISSIVE FOR ALL TO wamn_platform
    USING (true) WITH CHECK (true);
CREATE INDEX workflows_tkey ON catalog.workflows ((wamn_authority.tenant_key(tenant_id)));

REVOKE ALL ON catalog.workflows FROM PUBLIC;
GRANT SELECT ON catalog.workflows TO wamn_app;
