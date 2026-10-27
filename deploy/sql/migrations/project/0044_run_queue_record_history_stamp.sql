-- Record stamps for run-queue (wamn-jesy3.3). The runner owns the transaction.
-- Existing actors are unknown, so both actors name wamn:provisioning.
-- Preserve each existing row time listed below, or use the transaction time.
-- Both times start equal. Disable immutable update guards only for the backfill.
-- Temporarily remove FORCE RLS so the owning migration role sees every row.
-- This also serves run-plane reconciliation and skips absent relations.
DO $history$
DECLARE
    item record;
    was_forced boolean;
BEGIN
    PERFORM pg_catalog.set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', true);
    FOR item IN SELECT * FROM (VALUES
        ('wamn_run.run_queue', 'enqueued_at', NULL)
    ) AS relations (relation, history_time, immutable_trigger)
    LOOP
        IF to_regclass(item.relation) IS NULL THEN
            CONTINUE;
        END IF;
        EXECUTE format('ALTER TABLE %s ADD COLUMN IF NOT EXISTS created_at timestamptz, '
            'ADD COLUMN IF NOT EXISTS created_by uuid, ADD COLUMN IF NOT EXISTS updated_at timestamptz, '
            'ADD COLUMN IF NOT EXISTS updated_by uuid', item.relation);
        SELECT relforcerowsecurity INTO was_forced FROM pg_class WHERE oid = item.relation::regclass;
        IF was_forced THEN
            EXECUTE format('ALTER TABLE %s NO FORCE ROW LEVEL SECURITY', item.relation);
        END IF;
        IF item.immutable_trigger IS NOT NULL THEN
            EXECUTE format('ALTER TABLE %s DISABLE TRIGGER %I', item.relation, item.immutable_trigger);
        END IF;
        EXECUTE format('UPDATE %s SET created_at = COALESCE(created_at, %s), '
            'updated_at = COALESCE(updated_at, %s), '
            'created_by = COALESCE(created_by, %L::uuid), updated_by = COALESCE(updated_by, %L::uuid) '
            'WHERE created_at IS NULL OR created_by IS NULL OR updated_at IS NULL OR updated_by IS NULL',
            item.relation, item.history_time, item.history_time,
            '770df186-ac15-579e-b46b-c297cae2011b', '770df186-ac15-579e-b46b-c297cae2011b');
        IF item.immutable_trigger IS NOT NULL THEN
            EXECUTE format('ALTER TABLE %s ENABLE TRIGGER %I', item.relation, item.immutable_trigger);
        END IF;
        IF was_forced THEN
            EXECUTE format('ALTER TABLE %s FORCE ROW LEVEL SECURITY', item.relation);
        END IF;
        EXECUTE format('ALTER TABLE %s ALTER COLUMN created_at DROP DEFAULT, '
            'ALTER COLUMN created_at SET NOT NULL, ALTER COLUMN created_by SET NOT NULL, '
            'ALTER COLUMN updated_at SET NOT NULL, ALTER COLUMN updated_by SET NOT NULL', item.relation);
        EXECUTE format('DROP TRIGGER IF EXISTS wamn_record_history_stamp ON %s', item.relation);
        EXECUTE format('CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON %s '
            'FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row('
            '''created_at'', ''created_by'', ''updated_at'', ''updated_by'')', item.relation);
    END LOOP;
END
$history$;
