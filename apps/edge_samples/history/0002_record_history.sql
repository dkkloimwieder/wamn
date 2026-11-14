-- wamn-authored-migration-versions: [1]
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "edge_samples"."sample";
CREATE OR REPLACE FUNCTION wamn_history.edge_samples__sample_stamp()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $stamp$
DECLARE
    actor uuid := NULLIF(current_setting('app.user_id', true), '')::uuid;
BEGIN
    IF actor IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'actor-required';
    END IF;
    IF TG_OP = 'INSERT' THEN
        NEW."created_at" := transaction_timestamp();
    ELSIF (NEW."id"::text IS DISTINCT FROM OLD."id"::text
            OR NEW."frame"::text IS DISTINCT FROM OLD."frame"::text
            OR NEW."captured_at"::text IS DISTINCT FROM OLD."captured_at"::text) THEN
        NEW."created_at" := OLD."created_at";
    ELSE
        NEW."created_at" := OLD."created_at";
    END IF;
    RETURN NEW;
END
$stamp$;;
CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON edge_samples.sample FOR EACH ROW EXECUTE FUNCTION wamn_history.edge_samples__sample_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "edge_samples"."sample";
DROP FUNCTION IF EXISTS wamn_history.edge_samples__sample_log();
