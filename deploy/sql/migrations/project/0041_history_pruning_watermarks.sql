-- Keep the pruning boundary after the last history entry disappears.

-- The highest transaction id removed from each history table. A table has no
-- row until pruning removes an entry. The relation OID distinguishes a new
-- history table from a dropped table with the same name.
CREATE TABLE IF NOT EXISTS wamn_history.pruning_watermarks (
    history_table regclass PRIMARY KEY,
    transaction_id bigint NOT NULL
);
REVOKE ALL ON TABLE wamn_history.pruning_watermarks FROM PUBLIC;

GRANT SELECT ON TABLE wamn_history.pruning_watermarks TO wamn_app;

-- Existing retention credentials need the new read and watermark writes before
-- their next prune. Keep the same lock and target selection as reconciliation.
SELECT pg_advisory_xact_lock(hashtextextended('wamn.audit-retention', 0));
DO $watermark_grants$
DECLARE
    target record;
BEGIN
    IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_audit_retention') THEN
        RETURN;
    END IF;
    FOR target IN
SELECT log.schema_name, log.relation_name, log.history_name, log.days
  FROM (
        SELECT namespace.nspname::text AS schema_name,
               relation.relname::text AS relation_name,
               relation.relnamespace,
               relation.relname::text || '_history' AS history_name,
               substring(pg_catalog.encode(trigger.tgargs, 'escape')
                         FROM '^P([1-9][0-9]*)D\\000$') AS days
          FROM pg_catalog.pg_trigger AS trigger
          JOIN pg_catalog.pg_class AS relation ON relation.oid = trigger.tgrelid
          JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace
          JOIN pg_catalog.pg_proc AS routine ON routine.oid = trigger.tgfoid
          JOIN pg_catalog.pg_namespace AS routine_namespace
            ON routine_namespace.oid = routine.pronamespace
         WHERE NOT trigger.tgisinternal
           AND trigger.tgname = 'wamn_record_history_log'
           AND trigger.tgnargs = 1
           AND routine_namespace.nspname = 'wamn_history'
       ) AS log
 WHERE log.days IS NOT NULL
   AND EXISTS (SELECT FROM pg_catalog.pg_class AS history
                WHERE history.relnamespace = log.relnamespace
                  AND history.relname = log.history_name
                  AND history.relkind = 'r')
 ORDER BY log.schema_name COLLATE "C", log.relation_name COLLATE "C"
    LOOP
        EXECUTE format('GRANT SELECT (transaction_id) ON TABLE %I.%I TO wamn_audit_retention',
                       target.schema_name, target.history_name);
        GRANT USAGE ON SCHEMA wamn_history TO wamn_audit_retention;
        GRANT SELECT, INSERT, UPDATE ON TABLE wamn_history.pruning_watermarks TO wamn_audit_retention;
    END LOOP;
END
$watermark_grants$;
