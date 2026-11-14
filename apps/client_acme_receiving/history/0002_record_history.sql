-- wamn-authored-migration-versions: [1]
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "receiving"."quality_inspection";
CREATE OR REPLACE FUNCTION wamn_history.receiving__quality_inspection_stamp()
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
        NEW."created_by" := actor;
        NEW."updated_at" := transaction_timestamp();
        NEW."updated_by" := actor;
    ELSIF (NEW."receipt_id"::text IS DISTINCT FROM OLD."receipt_id"::text
            OR NEW."status"::text IS DISTINCT FROM OLD."status"::text
            OR NEW."row_version"::text IS DISTINCT FROM OLD."row_version"::text) THEN
        NEW."created_at" := OLD."created_at";
        NEW."created_by" := OLD."created_by";
        NEW."updated_at" := transaction_timestamp();
        NEW."updated_by" := actor;
    ELSE
        NEW."created_at" := OLD."created_at";
        NEW."created_by" := OLD."created_by";
        NEW."updated_at" := OLD."updated_at";
        NEW."updated_by" := OLD."updated_by";
    END IF;
    RETURN NEW;
END
$stamp$;;
CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON receiving.quality_inspection FOR EACH ROW EXECUTE FUNCTION wamn_history.receiving__quality_inspection_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "receiving"."quality_inspection";
DROP FUNCTION IF EXISTS wamn_history.receiving__quality_inspection_log();
