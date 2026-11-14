-- wamn-authored-migration-versions: [1]
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "wamn_integration"."log";
DROP FUNCTION IF EXISTS wamn_history.wamn_integration__log_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "wamn_integration"."log";
DROP FUNCTION IF EXISTS wamn_history.wamn_integration__log_log();
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "wamn_integration"."setting";
DROP FUNCTION IF EXISTS wamn_history.wamn_integration__setting_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "wamn_integration"."setting";
DROP FUNCTION IF EXISTS wamn_history.wamn_integration__setting_log();
