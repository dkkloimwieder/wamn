-- wamn-authored-migration-versions: [1]
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "rest_consumer_fixture"."item";
DROP FUNCTION IF EXISTS wamn_history.rest_consumer_fixture__item_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "rest_consumer_fixture"."item";
DROP FUNCTION IF EXISTS wamn_history.rest_consumer_fixture__item_log();
