-- Stamp the system relations named by wamn-jesy3.1. The runner owns the transaction.
-- Existing actors are unknown, so both actor fields name wamn:provisioning.
-- Preserve capture_gap.created_at. Use retired_at, recorded_at, published_at,
-- and started_at for the corresponding tables. Use transaction time elsewhere.
-- Both times start equal. The backfill precedes NOT NULL constraints and triggers.
DO $history$
DECLARE
    item record;
BEGIN
    PERFORM pg_catalog.set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', true);
    FOR item IN SELECT * FROM (VALUES
        ('registry.meta', 'transaction_timestamp()'),
        ('registry.orgs', 'transaction_timestamp()'),
        ('registry.env_policies', 'transaction_timestamp()'),
        ('registry.projects', 'transaction_timestamp()'),
        ('registry.project_envs', 'transaction_timestamp()'),
        ('registry.retired_project_envs', 'retired_at'),
        ('registry.capture_gap', 'created_at'),
        ('registry.generation_passwords', 'recorded_at'),
        ('identity.session_keys', 'published_at'),
        ('identity.session_signing_state', 'transaction_timestamp()'),
        ('identity.password_attempts', 'started_at')
    ) AS relations (relation, history_time)
    LOOP
        IF item.relation <> 'registry.capture_gap' THEN
            EXECUTE format('ALTER TABLE %s ADD COLUMN created_at timestamptz', item.relation);
        END IF;
        EXECUTE format('ALTER TABLE %s ADD COLUMN created_by uuid, '
            'ADD COLUMN updated_at timestamptz, ADD COLUMN updated_by uuid', item.relation);
        EXECUTE format('UPDATE %s SET created_at = %s, updated_at = %s, '
            'created_by = %L::uuid, updated_by = %L::uuid',
            item.relation, item.history_time, item.history_time,
            '770df186-ac15-579e-b46b-c297cae2011b', '770df186-ac15-579e-b46b-c297cae2011b');
        EXECUTE format('ALTER TABLE %s ALTER COLUMN created_at DROP DEFAULT, '
            'ALTER COLUMN created_at SET NOT NULL, ALTER COLUMN created_by SET NOT NULL, '
            'ALTER COLUMN updated_at SET NOT NULL, ALTER COLUMN updated_by SET NOT NULL', item.relation);
        EXECUTE format('CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON %s '
            'FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row('
            '''created_at'', ''created_by'', ''updated_at'', ''updated_by'')', item.relation);
    END LOOP;
END
$history$;
