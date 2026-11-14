-- wamn-authored-migration-versions: [1]
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "wms"."inventory_transaction";
CREATE OR REPLACE FUNCTION wamn_history.wms__inventory_transaction_stamp()
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
    ELSIF (NEW."id"::text IS DISTINCT FROM OLD."id"::text
            OR NEW."product_id"::text IS DISTINCT FROM OLD."product_id"::text
            OR NEW."quantity"::text IS DISTINCT FROM OLD."quantity"::text
            OR NEW."from_packaging_id"::text IS DISTINCT FROM OLD."from_packaging_id"::text
            OR NEW."from_status"::text IS DISTINCT FROM OLD."from_status"::text
            OR NEW."to_packaging_id"::text IS DISTINCT FROM OLD."to_packaging_id"::text
            OR NEW."to_status"::text IS DISTINCT FROM OLD."to_status"::text
            OR NEW."occurred_at"::text IS DISTINCT FROM OLD."occurred_at"::text
            OR NEW."reason_code"::text IS DISTINCT FROM OLD."reason_code"::text) THEN
        NEW."created_at" := OLD."created_at";
        NEW."created_by" := OLD."created_by";
    ELSE
        NEW."created_at" := OLD."created_at";
        NEW."created_by" := OLD."created_by";
    END IF;
    RETURN NEW;
END
$stamp$;;
CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON wms.inventory_transaction FOR EACH ROW EXECUTE FUNCTION wamn_history.wms__inventory_transaction_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "wms"."inventory_transaction";
DROP FUNCTION IF EXISTS wamn_history.wms__inventory_transaction_log();
SELECT wamn_history.create_history_table('wms', 'location', false);
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "wms"."location";
CREATE OR REPLACE FUNCTION wamn_history.wms__location_stamp()
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
            OR NEW."location_code"::text IS DISTINCT FROM OLD."location_code"::text
            OR NEW."row_version"::text IS DISTINCT FROM OLD."row_version"::text
            OR NEW."description"::text IS DISTINCT FROM OLD."description"::text) THEN
        NEW."created_at" := OLD."created_at";
    ELSE
        NEW."created_at" := OLD."created_at";
    END IF;
    RETURN NEW;
END
$stamp$;;
CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON wms.location FOR EACH ROW EXECUTE FUNCTION wamn_history.wms__location_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "wms"."location";
CREATE OR REPLACE FUNCTION wamn_history.wms__location_log()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $log$
DECLARE
    actor uuid := NULLIF(current_setting('app.user_id', true), '')::uuid;
    operation text := NULLIF(current_setting('app.operation', true), '');
    changed_before jsonb := '{}';
    changed_after jsonb := '{}';
BEGIN
    IF actor IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'actor-required';
    END IF;
    IF operation IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'operation-required';
    END IF;
    IF TG_OP = 'UPDATE' THEN
        -- A true no-op writes no entry.
        IF NOT (NEW."id"::text IS DISTINCT FROM OLD."id"::text
            OR NEW."location_code"::text IS DISTINCT FROM OLD."location_code"::text
            OR NEW."row_version"::text IS DISTINCT FROM OLD."row_version"::text
            OR NEW."created_at"::text IS DISTINCT FROM OLD."created_at"::text
            OR NEW."description"::text IS DISTINCT FROM OLD."description"::text) THEN
            RETURN NULL;
        END IF;
        IF to_jsonb(OLD."id") = to_jsonb(NEW."id") THEN
            IF NEW."id"::text IS DISTINCT FROM OLD."id"::text THEN
                changed_before := changed_before || jsonb_build_object('id', OLD."id");
                changed_after := changed_after || jsonb_build_object('id', NEW."id");
            END IF;
            IF NEW."location_code"::text IS DISTINCT FROM OLD."location_code"::text THEN
                changed_before := changed_before || jsonb_build_object('location_code', OLD."location_code");
                changed_after := changed_after || jsonb_build_object('location_code', NEW."location_code");
            END IF;
            IF NEW."row_version"::text IS DISTINCT FROM OLD."row_version"::text THEN
                changed_before := changed_before || jsonb_build_object('row_version', OLD."row_version");
                changed_after := changed_after || jsonb_build_object('row_version', NEW."row_version");
            END IF;
            IF NEW."created_at"::text IS DISTINCT FROM OLD."created_at"::text THEN
                changed_before := changed_before || jsonb_build_object('created_at', OLD."created_at");
                changed_after := changed_after || jsonb_build_object('created_at', NEW."created_at");
            END IF;
            IF NEW."description"::text IS DISTINCT FROM OLD."description"::text THEN
                changed_before := changed_before || jsonb_build_object('description', OLD."description");
                changed_after := changed_after || jsonb_build_object('description', NEW."description");
            END IF;
            INSERT INTO "wms"."location_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
            VALUES (jsonb_build_object('id', NEW."id"), 'update', operation, actor, transaction_timestamp(),
                    pg_current_xact_id()::text::bigint, changed_before, changed_after);
            RETURN NULL;
        END IF;
    END IF;
    IF TG_OP <> 'INSERT' THEN
        INSERT INTO "wms"."location_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', OLD."id"), 'delete', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, to_jsonb(OLD), '{}');
    END IF;
    IF TG_OP <> 'DELETE' THEN
        INSERT INTO "wms"."location_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', NEW."id"), 'insert', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, '{}', to_jsonb(NEW));
    END IF;
    RETURN NULL;
END
$log$;;
CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE ON wms.location FOR EACH ROW EXECUTE FUNCTION wamn_history.wms__location_log('unlimited');
SELECT wamn_history.create_history_table('wms', 'packaging', false);
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "wms"."packaging";
CREATE OR REPLACE FUNCTION wamn_history.wms__packaging_stamp()
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
    ELSIF (NEW."id"::text IS DISTINCT FROM OLD."id"::text
            OR NEW."packaging_code"::text IS DISTINCT FROM OLD."packaging_code"::text
            OR NEW."location_id"::text IS DISTINCT FROM OLD."location_id"::text
            OR NEW."located_at"::text IS DISTINCT FROM OLD."located_at"::text
            OR NEW."type"::text IS DISTINCT FROM OLD."type"::text
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
CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON wms.packaging FOR EACH ROW EXECUTE FUNCTION wamn_history.wms__packaging_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "wms"."packaging";
CREATE OR REPLACE FUNCTION wamn_history.wms__packaging_log()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $log$
DECLARE
    actor uuid := NULLIF(current_setting('app.user_id', true), '')::uuid;
    operation text := NULLIF(current_setting('app.operation', true), '');
    changed_before jsonb := '{}';
    changed_after jsonb := '{}';
BEGIN
    IF actor IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'actor-required';
    END IF;
    IF operation IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'operation-required';
    END IF;
    IF TG_OP = 'UPDATE' THEN
        -- A true no-op writes no entry.
        IF NOT (NEW."id"::text IS DISTINCT FROM OLD."id"::text
            OR NEW."packaging_code"::text IS DISTINCT FROM OLD."packaging_code"::text
            OR NEW."location_id"::text IS DISTINCT FROM OLD."location_id"::text
            OR NEW."located_at"::text IS DISTINCT FROM OLD."located_at"::text
            OR NEW."type"::text IS DISTINCT FROM OLD."type"::text
            OR NEW."status"::text IS DISTINCT FROM OLD."status"::text
            OR NEW."row_version"::text IS DISTINCT FROM OLD."row_version"::text
            OR NEW."created_at"::text IS DISTINCT FROM OLD."created_at"::text
            OR NEW."created_by"::text IS DISTINCT FROM OLD."created_by"::text
            OR NEW."updated_at"::text IS DISTINCT FROM OLD."updated_at"::text
            OR NEW."updated_by"::text IS DISTINCT FROM OLD."updated_by"::text) THEN
            RETURN NULL;
        END IF;
        IF to_jsonb(OLD."id") = to_jsonb(NEW."id") THEN
            IF NEW."id"::text IS DISTINCT FROM OLD."id"::text THEN
                changed_before := changed_before || jsonb_build_object('id', OLD."id");
                changed_after := changed_after || jsonb_build_object('id', NEW."id");
            END IF;
            IF NEW."packaging_code"::text IS DISTINCT FROM OLD."packaging_code"::text THEN
                changed_before := changed_before || jsonb_build_object('packaging_code', OLD."packaging_code");
                changed_after := changed_after || jsonb_build_object('packaging_code', NEW."packaging_code");
            END IF;
            IF NEW."location_id"::text IS DISTINCT FROM OLD."location_id"::text THEN
                changed_before := changed_before || jsonb_build_object('location_id', OLD."location_id");
                changed_after := changed_after || jsonb_build_object('location_id', NEW."location_id");
            END IF;
            IF NEW."located_at"::text IS DISTINCT FROM OLD."located_at"::text THEN
                changed_before := changed_before || jsonb_build_object('located_at', OLD."located_at");
                changed_after := changed_after || jsonb_build_object('located_at', NEW."located_at");
            END IF;
            IF NEW."type"::text IS DISTINCT FROM OLD."type"::text THEN
                changed_before := changed_before || jsonb_build_object('type', OLD."type");
                changed_after := changed_after || jsonb_build_object('type', NEW."type");
            END IF;
            IF NEW."status"::text IS DISTINCT FROM OLD."status"::text THEN
                changed_before := changed_before || jsonb_build_object('status', OLD."status");
                changed_after := changed_after || jsonb_build_object('status', NEW."status");
            END IF;
            IF NEW."row_version"::text IS DISTINCT FROM OLD."row_version"::text THEN
                changed_before := changed_before || jsonb_build_object('row_version', OLD."row_version");
                changed_after := changed_after || jsonb_build_object('row_version', NEW."row_version");
            END IF;
            IF NEW."created_at"::text IS DISTINCT FROM OLD."created_at"::text THEN
                changed_before := changed_before || jsonb_build_object('created_at', OLD."created_at");
                changed_after := changed_after || jsonb_build_object('created_at', NEW."created_at");
            END IF;
            IF NEW."created_by"::text IS DISTINCT FROM OLD."created_by"::text THEN
                changed_before := changed_before || jsonb_build_object('created_by', OLD."created_by");
                changed_after := changed_after || jsonb_build_object('created_by', NEW."created_by");
            END IF;
            IF NEW."updated_at"::text IS DISTINCT FROM OLD."updated_at"::text THEN
                changed_before := changed_before || jsonb_build_object('updated_at', OLD."updated_at");
                changed_after := changed_after || jsonb_build_object('updated_at', NEW."updated_at");
            END IF;
            IF NEW."updated_by"::text IS DISTINCT FROM OLD."updated_by"::text THEN
                changed_before := changed_before || jsonb_build_object('updated_by', OLD."updated_by");
                changed_after := changed_after || jsonb_build_object('updated_by', NEW."updated_by");
            END IF;
            INSERT INTO "wms"."packaging_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
            VALUES (jsonb_build_object('id', NEW."id"), 'update', operation, actor, transaction_timestamp(),
                    pg_current_xact_id()::text::bigint, changed_before, changed_after);
            RETURN NULL;
        END IF;
    END IF;
    IF TG_OP <> 'INSERT' THEN
        INSERT INTO "wms"."packaging_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', OLD."id"), 'delete', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, to_jsonb(OLD), '{}');
    END IF;
    IF TG_OP <> 'DELETE' THEN
        INSERT INTO "wms"."packaging_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', NEW."id"), 'insert', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, '{}', to_jsonb(NEW));
    END IF;
    RETURN NULL;
END
$log$;;
CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE ON wms.packaging FOR EACH ROW EXECUTE FUNCTION wamn_history.wms__packaging_log('unlimited');
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "wms"."packaging_quantity";
CREATE OR REPLACE FUNCTION wamn_history.wms__packaging_quantity_stamp()
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
            OR NEW."packaging_id"::text IS DISTINCT FROM OLD."packaging_id"::text
            OR NEW."product_id"::text IS DISTINCT FROM OLD."product_id"::text
            OR NEW."status"::text IS DISTINCT FROM OLD."status"::text
            OR NEW."quantity"::text IS DISTINCT FROM OLD."quantity"::text) THEN
        NEW."created_at" := OLD."created_at";
    ELSE
        NEW."created_at" := OLD."created_at";
    END IF;
    RETURN NEW;
END
$stamp$;;
CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON wms.packaging_quantity FOR EACH ROW EXECUTE FUNCTION wamn_history.wms__packaging_quantity_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "wms"."packaging_quantity";
DROP FUNCTION IF EXISTS wamn_history.wms__packaging_quantity_log();
SELECT wamn_history.create_history_table('wms', 'product', false);
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "wms"."product";
CREATE OR REPLACE FUNCTION wamn_history.wms__product_stamp()
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
            OR NEW."product_code"::text IS DISTINCT FROM OLD."product_code"::text
            OR NEW."row_version"::text IS DISTINCT FROM OLD."row_version"::text) THEN
        NEW."created_at" := OLD."created_at";
    ELSE
        NEW."created_at" := OLD."created_at";
    END IF;
    RETURN NEW;
END
$stamp$;;
CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON wms.product FOR EACH ROW EXECUTE FUNCTION wamn_history.wms__product_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "wms"."product";
CREATE OR REPLACE FUNCTION wamn_history.wms__product_log()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $log$
DECLARE
    actor uuid := NULLIF(current_setting('app.user_id', true), '')::uuid;
    operation text := NULLIF(current_setting('app.operation', true), '');
    changed_before jsonb := '{}';
    changed_after jsonb := '{}';
BEGIN
    IF actor IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'actor-required';
    END IF;
    IF operation IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'operation-required';
    END IF;
    IF TG_OP = 'UPDATE' THEN
        -- A true no-op writes no entry.
        IF NOT (NEW."id"::text IS DISTINCT FROM OLD."id"::text
            OR NEW."product_code"::text IS DISTINCT FROM OLD."product_code"::text
            OR NEW."row_version"::text IS DISTINCT FROM OLD."row_version"::text
            OR NEW."created_at"::text IS DISTINCT FROM OLD."created_at"::text) THEN
            RETURN NULL;
        END IF;
        IF to_jsonb(OLD."id") = to_jsonb(NEW."id") THEN
            IF NEW."id"::text IS DISTINCT FROM OLD."id"::text THEN
                changed_before := changed_before || jsonb_build_object('id', OLD."id");
                changed_after := changed_after || jsonb_build_object('id', NEW."id");
            END IF;
            IF NEW."product_code"::text IS DISTINCT FROM OLD."product_code"::text THEN
                changed_before := changed_before || jsonb_build_object('product_code', OLD."product_code");
                changed_after := changed_after || jsonb_build_object('product_code', NEW."product_code");
            END IF;
            IF NEW."row_version"::text IS DISTINCT FROM OLD."row_version"::text THEN
                changed_before := changed_before || jsonb_build_object('row_version', OLD."row_version");
                changed_after := changed_after || jsonb_build_object('row_version', NEW."row_version");
            END IF;
            IF NEW."created_at"::text IS DISTINCT FROM OLD."created_at"::text THEN
                changed_before := changed_before || jsonb_build_object('created_at', OLD."created_at");
                changed_after := changed_after || jsonb_build_object('created_at', NEW."created_at");
            END IF;
            INSERT INTO "wms"."product_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
            VALUES (jsonb_build_object('id', NEW."id"), 'update', operation, actor, transaction_timestamp(),
                    pg_current_xact_id()::text::bigint, changed_before, changed_after);
            RETURN NULL;
        END IF;
    END IF;
    IF TG_OP <> 'INSERT' THEN
        INSERT INTO "wms"."product_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', OLD."id"), 'delete', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, to_jsonb(OLD), '{}');
    END IF;
    IF TG_OP <> 'DELETE' THEN
        INSERT INTO "wms"."product_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', NEW."id"), 'insert', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, '{}', to_jsonb(NEW));
    END IF;
    RETURN NULL;
END
$log$;;
CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE ON wms.product FOR EACH ROW EXECUTE FUNCTION wamn_history.wms__product_log('unlimited');
