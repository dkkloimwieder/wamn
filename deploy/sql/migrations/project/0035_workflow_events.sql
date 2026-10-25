-- Event workflow definitions and their publication-order enable boundary.
ALTER TABLE catalog.workflows
    ADD COLUMN source_package text,
    ADD COLUMN entity text,
    ADD COLUMN ops text[],
    ADD COLUMN condition text,
    ADD COLUMN enable_sequence bigint;
ALTER TABLE catalog.workflows DROP CONSTRAINT workflows_trigger_check;
ALTER TABLE catalog.workflows ADD CONSTRAINT workflows_trigger_check
    CHECK (trigger IN ('manual', 'schedule', 'event'));
ALTER TABLE catalog.workflows ADD
CONSTRAINT workflows_event_check CHECK (
        (trigger = 'event' AND source_package IS NOT NULL AND source_package <> ''
         AND entity IS NOT NULL AND entity <> '' AND ops IS NOT NULL
         AND cardinality(ops) BETWEEN 1 AND 3
         AND ops <@ ARRAY['insert', 'update', 'delete']::text[]
         AND array_position(ops, NULL) IS NULL
         AND cardinality(array_positions(ops, 'insert')) <= 1
         AND cardinality(array_positions(ops, 'update')) <= 1
         AND cardinality(array_positions(ops, 'delete')) <= 1
         AND (condition IS NULL OR btrim(condition) <> '')
         AND (enable_sequence IS NULL OR enable_sequence >= 0)
         AND (NOT enabled OR enable_sequence IS NOT NULL))
        OR (trigger <> 'event' AND source_package IS NULL AND entity IS NULL
            AND ops IS NULL AND condition IS NULL AND enable_sequence IS NULL));

CREATE OR REPLACE FUNCTION catalog.guard_workflow_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF ROW(NEW.tenant_id, NEW.environment, NEW.application, NEW.workflow_id,
           NEW.version, NEW.graph_json, NEW.wiring_hash, NEW.trigger, NEW.schedule,
           NEW.source_package, NEW.entity, NEW.ops, NEW.condition,
           NEW.execution_type, NEW.execution_role, NEW.created_by, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.tenant_id, OLD.environment, OLD.application, OLD.workflow_id,
           OLD.version, OLD.graph_json, OLD.wiring_hash, OLD.trigger, OLD.schedule,
           OLD.source_package, OLD.entity, OLD.ops, OLD.condition,
           OLD.execution_type, OLD.execution_role, OLD.created_by, OLD.created_at) THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'workflow-version-immutable';
    END IF;
    IF NEW.enable_sequence IS DISTINCT FROM OLD.enable_sequence AND NOT NEW.enabled THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'workflow-enable-sequence-requires-enable';
    END IF;
    RETURN NEW;
END
$$;
