-- The schedule trigger of catalog.workflows (docs/plan/workflows.md R3, R8,
-- epic 2) in an installed project database. The column, its checks and the
-- immutable-row guard are the ones deploy/sql/catalog-schema.sql creates.
-- upgrade-schema runs this file once in one transaction and records it. A
-- fresh install records it as applied, because catalog-schema.sql already
-- holds the column.

ALTER TABLE catalog.workflows ADD COLUMN schedule text;
ALTER TABLE catalog.workflows DROP CONSTRAINT workflows_trigger_check;
ALTER TABLE catalog.workflows
    ADD CONSTRAINT workflows_trigger_check CHECK (trigger IN ('manual', 'schedule'));
ALTER TABLE catalog.workflows
    ADD CONSTRAINT workflows_schedule_check
        CHECK ((trigger = 'schedule') = (schedule IS NOT NULL));

CREATE OR REPLACE FUNCTION catalog.guard_workflow_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF ROW(NEW.tenant_id, NEW.environment, NEW.application, NEW.workflow_id,
           NEW.version, NEW.graph_json, NEW.wiring_hash, NEW.trigger, NEW.schedule,
           NEW.execution_type, NEW.execution_role, NEW.created_by, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.tenant_id, OLD.environment, OLD.application, OLD.workflow_id,
           OLD.version, OLD.graph_json, OLD.wiring_hash, OLD.trigger, OLD.schedule,
           OLD.execution_type, OLD.execution_role, OLD.created_by, OLD.created_at) THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'workflow-version-immutable';
    END IF;
    RETURN NEW;
END
$$;
