-- wamn-authored-migration-versions: [1]
SELECT wamn_history.create_history_table('receiving', 'item', false);
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "receiving"."item";
DROP FUNCTION IF EXISTS wamn_history.receiving__item_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "receiving"."item";
CREATE OR REPLACE FUNCTION wamn_history.receiving__item_log()
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
            OR NEW."item_number"::text IS DISTINCT FROM OLD."item_number"::text) THEN
            RETURN NULL;
        END IF;
        IF to_jsonb(OLD."id") = to_jsonb(NEW."id") THEN
            IF NEW."id"::text IS DISTINCT FROM OLD."id"::text THEN
                changed_before := changed_before || jsonb_build_object('id', OLD."id");
                changed_after := changed_after || jsonb_build_object('id', NEW."id");
            END IF;
            IF NEW."item_number"::text IS DISTINCT FROM OLD."item_number"::text THEN
                changed_before := changed_before || jsonb_build_object('item_number', OLD."item_number");
                changed_after := changed_after || jsonb_build_object('item_number', NEW."item_number");
            END IF;
            INSERT INTO "receiving"."item_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
            VALUES (jsonb_build_object('id', NEW."id"), 'update', operation, actor, transaction_timestamp(),
                    pg_current_xact_id()::text::bigint, changed_before, changed_after);
            RETURN NULL;
        END IF;
    END IF;
    IF TG_OP <> 'INSERT' THEN
        INSERT INTO "receiving"."item_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', OLD."id"), 'delete', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, to_jsonb(OLD), '{}');
    END IF;
    IF TG_OP <> 'DELETE' THEN
        INSERT INTO "receiving"."item_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', NEW."id"), 'insert', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, '{}', to_jsonb(NEW));
    END IF;
    RETURN NULL;
END
$log$;;
CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE ON receiving.item FOR EACH ROW EXECUTE FUNCTION wamn_history.receiving__item_log('P30D');
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "receiving"."location";
DROP FUNCTION IF EXISTS wamn_history.receiving__location_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "receiving"."location";
DROP FUNCTION IF EXISTS wamn_history.receiving__location_log();
SELECT wamn_history.create_history_table('receiving', 'purchase_order', false);
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "receiving"."purchase_order";
CREATE OR REPLACE FUNCTION wamn_history.receiving__purchase_order_stamp()
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
            OR NEW."purchase_order_number"::text IS DISTINCT FROM OLD."purchase_order_number"::text
            OR NEW."supplier_id"::text IS DISTINCT FROM OLD."supplier_id"::text
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
CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON receiving.purchase_order FOR EACH ROW EXECUTE FUNCTION wamn_history.receiving__purchase_order_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "receiving"."purchase_order";
CREATE OR REPLACE FUNCTION wamn_history.receiving__purchase_order_log()
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
            OR NEW."purchase_order_number"::text IS DISTINCT FROM OLD."purchase_order_number"::text
            OR NEW."supplier_id"::text IS DISTINCT FROM OLD."supplier_id"::text
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
            IF NEW."purchase_order_number"::text IS DISTINCT FROM OLD."purchase_order_number"::text THEN
                changed_before := changed_before || jsonb_build_object('purchase_order_number', OLD."purchase_order_number");
                changed_after := changed_after || jsonb_build_object('purchase_order_number', NEW."purchase_order_number");
            END IF;
            IF NEW."supplier_id"::text IS DISTINCT FROM OLD."supplier_id"::text THEN
                changed_before := changed_before || jsonb_build_object('supplier_id', OLD."supplier_id");
                changed_after := changed_after || jsonb_build_object('supplier_id', NEW."supplier_id");
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
            INSERT INTO "receiving"."purchase_order_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
            VALUES (jsonb_build_object('id', NEW."id"), 'update', operation, actor, transaction_timestamp(),
                    pg_current_xact_id()::text::bigint, changed_before, changed_after);
            RETURN NULL;
        END IF;
    END IF;
    IF TG_OP <> 'INSERT' THEN
        INSERT INTO "receiving"."purchase_order_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', OLD."id"), 'delete', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, to_jsonb(OLD), '{}');
    END IF;
    IF TG_OP <> 'DELETE' THEN
        INSERT INTO "receiving"."purchase_order_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', NEW."id"), 'insert', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, '{}', to_jsonb(NEW));
    END IF;
    RETURN NULL;
END
$log$;;
CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE ON receiving.purchase_order FOR EACH ROW EXECUTE FUNCTION wamn_history.receiving__purchase_order_log('unlimited');
SELECT wamn_history.create_history_table('receiving', 'purchase_order_line', false);
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "receiving"."purchase_order_line";
DROP FUNCTION IF EXISTS wamn_history.receiving__purchase_order_line_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "receiving"."purchase_order_line";
CREATE OR REPLACE FUNCTION wamn_history.receiving__purchase_order_line_log()
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
            OR NEW."purchase_order_id"::text IS DISTINCT FROM OLD."purchase_order_id"::text
            OR NEW."line_number"::text IS DISTINCT FROM OLD."line_number"::text
            OR NEW."item_id"::text IS DISTINCT FROM OLD."item_id"::text
            OR NEW."ordered_quantity"::text IS DISTINCT FROM OLD."ordered_quantity"::text
            OR NEW."received_quantity"::text IS DISTINCT FROM OLD."received_quantity"::text) THEN
            RETURN NULL;
        END IF;
        IF to_jsonb(OLD."id") = to_jsonb(NEW."id") THEN
            IF NEW."id"::text IS DISTINCT FROM OLD."id"::text THEN
                changed_before := changed_before || jsonb_build_object('id', OLD."id");
                changed_after := changed_after || jsonb_build_object('id', NEW."id");
            END IF;
            IF NEW."purchase_order_id"::text IS DISTINCT FROM OLD."purchase_order_id"::text THEN
                changed_before := changed_before || jsonb_build_object('purchase_order_id', OLD."purchase_order_id");
                changed_after := changed_after || jsonb_build_object('purchase_order_id', NEW."purchase_order_id");
            END IF;
            IF NEW."line_number"::text IS DISTINCT FROM OLD."line_number"::text THEN
                changed_before := changed_before || jsonb_build_object('line_number', OLD."line_number");
                changed_after := changed_after || jsonb_build_object('line_number', NEW."line_number");
            END IF;
            IF NEW."item_id"::text IS DISTINCT FROM OLD."item_id"::text THEN
                changed_before := changed_before || jsonb_build_object('item_id', OLD."item_id");
                changed_after := changed_after || jsonb_build_object('item_id', NEW."item_id");
            END IF;
            IF NEW."ordered_quantity"::text IS DISTINCT FROM OLD."ordered_quantity"::text THEN
                changed_before := changed_before || jsonb_build_object('ordered_quantity', OLD."ordered_quantity");
                changed_after := changed_after || jsonb_build_object('ordered_quantity', NEW."ordered_quantity");
            END IF;
            IF NEW."received_quantity"::text IS DISTINCT FROM OLD."received_quantity"::text THEN
                changed_before := changed_before || jsonb_build_object('received_quantity', OLD."received_quantity");
                changed_after := changed_after || jsonb_build_object('received_quantity', NEW."received_quantity");
            END IF;
            INSERT INTO "receiving"."purchase_order_line_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
            VALUES (jsonb_build_object('id', NEW."id"), 'update', operation, actor, transaction_timestamp(),
                    pg_current_xact_id()::text::bigint, changed_before, changed_after);
            RETURN NULL;
        END IF;
    END IF;
    IF TG_OP <> 'INSERT' THEN
        INSERT INTO "receiving"."purchase_order_line_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', OLD."id"), 'delete', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, to_jsonb(OLD), '{}');
    END IF;
    IF TG_OP <> 'DELETE' THEN
        INSERT INTO "receiving"."purchase_order_line_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', NEW."id"), 'insert', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, '{}', to_jsonb(NEW));
    END IF;
    RETURN NULL;
END
$log$;;
CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE ON receiving.purchase_order_line FOR EACH ROW EXECUTE FUNCTION wamn_history.receiving__purchase_order_line_log('P30D');
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "receiving"."receipt";
CREATE OR REPLACE FUNCTION wamn_history.receiving__receipt_stamp()
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
            OR NEW."purchase_order_id"::text IS DISTINCT FROM OLD."purchase_order_id"::text
            OR NEW."receipt_reference"::text IS DISTINCT FROM OLD."receipt_reference"::text
            OR NEW."occurred_at"::text IS DISTINCT FROM OLD."occurred_at"::text) THEN
        NEW."created_at" := OLD."created_at";
        NEW."created_by" := OLD."created_by";
    ELSE
        NEW."created_at" := OLD."created_at";
        NEW."created_by" := OLD."created_by";
    END IF;
    RETURN NEW;
END
$stamp$;;
CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON receiving.receipt FOR EACH ROW EXECUTE FUNCTION wamn_history.receiving__receipt_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "receiving"."receipt";
DROP FUNCTION IF EXISTS wamn_history.receiving__receipt_log();
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "receiving"."receipt_line";
DROP FUNCTION IF EXISTS wamn_history.receiving__receipt_line_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "receiving"."receipt_line";
DROP FUNCTION IF EXISTS wamn_history.receiving__receipt_line_log();
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "receiving"."supplier";
CREATE OR REPLACE FUNCTION wamn_history.receiving__supplier_stamp()
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
            OR NEW."name"::text IS DISTINCT FROM OLD."name"::text) THEN
        NEW."created_at" := OLD."created_at";
    ELSE
        NEW."created_at" := OLD."created_at";
    END IF;
    RETURN NEW;
END
$stamp$;;
CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON receiving.supplier FOR EACH ROW EXECUTE FUNCTION wamn_history.receiving__supplier_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "receiving"."supplier";
DROP FUNCTION IF EXISTS wamn_history.receiving__supplier_log();
