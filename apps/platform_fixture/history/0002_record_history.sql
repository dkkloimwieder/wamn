-- wamn-authored-migration-versions: [1]
SELECT wamn_history.create_history_table('inventory', 'counter', false);
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "inventory"."counter";
CREATE OR REPLACE FUNCTION wamn_history.inventory__counter_stamp()
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
            OR NEW."label"::text IS DISTINCT FROM OLD."label"::text
            OR NEW."edit_version"::text IS DISTINCT FROM OLD."edit_version"::text) THEN
        NEW."created_at" := OLD."created_at";
    ELSE
        NEW."created_at" := OLD."created_at";
    END IF;
    RETURN NEW;
END
$stamp$;;
CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON inventory.counter FOR EACH ROW EXECUTE FUNCTION wamn_history.inventory__counter_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "inventory"."counter";
CREATE OR REPLACE FUNCTION wamn_history.inventory__counter_log()
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
            OR NEW."label"::text IS DISTINCT FROM OLD."label"::text
            OR NEW."edit_version"::text IS DISTINCT FROM OLD."edit_version"::text
            OR NEW."created_at"::text IS DISTINCT FROM OLD."created_at"::text) THEN
            RETURN NULL;
        END IF;
        IF to_jsonb(OLD."id") = to_jsonb(NEW."id") THEN
            IF NEW."id"::text IS DISTINCT FROM OLD."id"::text THEN
                changed_before := changed_before || jsonb_build_object('id', OLD."id");
                changed_after := changed_after || jsonb_build_object('id', NEW."id");
            END IF;
            IF NEW."label"::text IS DISTINCT FROM OLD."label"::text THEN
                changed_before := changed_before || jsonb_build_object('label', OLD."label");
                changed_after := changed_after || jsonb_build_object('label', NEW."label");
            END IF;
            IF NEW."edit_version"::text IS DISTINCT FROM OLD."edit_version"::text THEN
                changed_before := changed_before || jsonb_build_object('edit_version', OLD."edit_version");
                changed_after := changed_after || jsonb_build_object('edit_version', NEW."edit_version");
            END IF;
            IF NEW."created_at"::text IS DISTINCT FROM OLD."created_at"::text THEN
                changed_before := changed_before || jsonb_build_object('created_at', OLD."created_at");
                changed_after := changed_after || jsonb_build_object('created_at', NEW."created_at");
            END IF;
            INSERT INTO "inventory"."counter_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
            VALUES (jsonb_build_object('id', NEW."id"), 'update', operation, actor, transaction_timestamp(),
                    pg_current_xact_id()::text::bigint, changed_before, changed_after);
            RETURN NULL;
        END IF;
    END IF;
    IF TG_OP <> 'INSERT' THEN
        INSERT INTO "inventory"."counter_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', OLD."id"), 'delete', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, to_jsonb(OLD), '{}');
    END IF;
    IF TG_OP <> 'DELETE' THEN
        INSERT INTO "inventory"."counter_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', NEW."id"), 'insert', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, '{}', to_jsonb(NEW));
    END IF;
    RETURN NULL;
END
$log$;;
CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE ON inventory.counter FOR EACH ROW EXECUTE FUNCTION wamn_history.inventory__counter_log('P30D');
SELECT wamn_history.create_history_table('inventory', 'feed_item', false);
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "inventory"."feed_item";
DROP FUNCTION IF EXISTS wamn_history.inventory__feed_item_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "inventory"."feed_item";
CREATE OR REPLACE FUNCTION wamn_history.inventory__feed_item_log()
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
            OR NEW."label"::text IS DISTINCT FROM OLD."label"::text) THEN
            RETURN NULL;
        END IF;
        IF to_jsonb(OLD."id") = to_jsonb(NEW."id") THEN
            IF NEW."id"::text IS DISTINCT FROM OLD."id"::text THEN
                changed_before := changed_before || jsonb_build_object('id', OLD."id");
                changed_after := changed_after || jsonb_build_object('id', NEW."id");
            END IF;
            IF NEW."label"::text IS DISTINCT FROM OLD."label"::text THEN
                changed_before := changed_before || jsonb_build_object('label', OLD."label");
                changed_after := changed_after || jsonb_build_object('label', NEW."label");
            END IF;
            INSERT INTO "inventory"."feed_item_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
            VALUES (jsonb_build_object('id', NEW."id"), 'update', operation, actor, transaction_timestamp(),
                    pg_current_xact_id()::text::bigint, changed_before, changed_after);
            RETURN NULL;
        END IF;
    END IF;
    IF TG_OP <> 'INSERT' THEN
        INSERT INTO "inventory"."feed_item_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', OLD."id"), 'delete', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, to_jsonb(OLD), '{}');
    END IF;
    IF TG_OP <> 'DELETE' THEN
        INSERT INTO "inventory"."feed_item_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', NEW."id"), 'insert', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, '{}', to_jsonb(NEW));
    END IF;
    RETURN NULL;
END
$log$;;
CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE ON inventory.feed_item FOR EACH ROW EXECUTE FUNCTION wamn_history.inventory__feed_item_log('P30D');
SELECT wamn_history.create_history_table('inventory', 'feed_line', false);
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "inventory"."feed_line";
DROP FUNCTION IF EXISTS wamn_history.inventory__feed_line_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "inventory"."feed_line";
CREATE OR REPLACE FUNCTION wamn_history.inventory__feed_line_log()
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
            OR NEW."order_id"::text IS DISTINCT FROM OLD."order_id"::text
            OR NEW."item_id"::text IS DISTINCT FROM OLD."item_id"::text
            OR NEW."label"::text IS DISTINCT FROM OLD."label"::text
            OR NEW."enabled"::text IS DISTINCT FROM OLD."enabled"::text) THEN
            RETURN NULL;
        END IF;
        IF to_jsonb(OLD."id") = to_jsonb(NEW."id") THEN
            IF NEW."id"::text IS DISTINCT FROM OLD."id"::text THEN
                changed_before := changed_before || jsonb_build_object('id', OLD."id");
                changed_after := changed_after || jsonb_build_object('id', NEW."id");
            END IF;
            IF NEW."order_id"::text IS DISTINCT FROM OLD."order_id"::text THEN
                changed_before := changed_before || jsonb_build_object('order_id', OLD."order_id");
                changed_after := changed_after || jsonb_build_object('order_id', NEW."order_id");
            END IF;
            IF NEW."item_id"::text IS DISTINCT FROM OLD."item_id"::text THEN
                changed_before := changed_before || jsonb_build_object('item_id', OLD."item_id");
                changed_after := changed_after || jsonb_build_object('item_id', NEW."item_id");
            END IF;
            IF NEW."label"::text IS DISTINCT FROM OLD."label"::text THEN
                changed_before := changed_before || jsonb_build_object('label', OLD."label");
                changed_after := changed_after || jsonb_build_object('label', NEW."label");
            END IF;
            IF NEW."enabled"::text IS DISTINCT FROM OLD."enabled"::text THEN
                changed_before := changed_before || jsonb_build_object('enabled', OLD."enabled");
                changed_after := changed_after || jsonb_build_object('enabled', NEW."enabled");
            END IF;
            INSERT INTO "inventory"."feed_line_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
            VALUES (jsonb_build_object('id', NEW."id"), 'update', operation, actor, transaction_timestamp(),
                    pg_current_xact_id()::text::bigint, changed_before, changed_after);
            RETURN NULL;
        END IF;
    END IF;
    IF TG_OP <> 'INSERT' THEN
        INSERT INTO "inventory"."feed_line_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', OLD."id"), 'delete', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, to_jsonb(OLD), '{}');
    END IF;
    IF TG_OP <> 'DELETE' THEN
        INSERT INTO "inventory"."feed_line_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', NEW."id"), 'insert', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, '{}', to_jsonb(NEW));
    END IF;
    RETURN NULL;
END
$log$;;
CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE ON inventory.feed_line FOR EACH ROW EXECUTE FUNCTION wamn_history.inventory__feed_line_log('P30D');
SELECT wamn_history.create_history_table('inventory', 'feed_order', false);
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "inventory"."feed_order";
DROP FUNCTION IF EXISTS wamn_history.inventory__feed_order_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "inventory"."feed_order";
CREATE OR REPLACE FUNCTION wamn_history.inventory__feed_order_log()
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
            OR NEW."label"::text IS DISTINCT FROM OLD."label"::text) THEN
            RETURN NULL;
        END IF;
        IF to_jsonb(OLD."id") = to_jsonb(NEW."id") THEN
            IF NEW."id"::text IS DISTINCT FROM OLD."id"::text THEN
                changed_before := changed_before || jsonb_build_object('id', OLD."id");
                changed_after := changed_after || jsonb_build_object('id', NEW."id");
            END IF;
            IF NEW."label"::text IS DISTINCT FROM OLD."label"::text THEN
                changed_before := changed_before || jsonb_build_object('label', OLD."label");
                changed_after := changed_after || jsonb_build_object('label', NEW."label");
            END IF;
            INSERT INTO "inventory"."feed_order_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
            VALUES (jsonb_build_object('id', NEW."id"), 'update', operation, actor, transaction_timestamp(),
                    pg_current_xact_id()::text::bigint, changed_before, changed_after);
            RETURN NULL;
        END IF;
    END IF;
    IF TG_OP <> 'INSERT' THEN
        INSERT INTO "inventory"."feed_order_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', OLD."id"), 'delete', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, to_jsonb(OLD), '{}');
    END IF;
    IF TG_OP <> 'DELETE' THEN
        INSERT INTO "inventory"."feed_order_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', NEW."id"), 'insert', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, '{}', to_jsonb(NEW));
    END IF;
    RETURN NULL;
END
$log$;;
CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE ON inventory.feed_order FOR EACH ROW EXECUTE FUNCTION wamn_history.inventory__feed_order_log('P30D');
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "inventory"."numbered_label";
CREATE OR REPLACE FUNCTION wamn_history.inventory__numbered_label_stamp()
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
            OR NEW."number"::text IS DISTINCT FROM OLD."number"::text
            OR NEW."stock_id"::text IS DISTINCT FROM OLD."stock_id"::text
            OR NEW."edit_version"::text IS DISTINCT FROM OLD."edit_version"::text) THEN
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
CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON inventory.numbered_label FOR EACH ROW EXECUTE FUNCTION wamn_history.inventory__numbered_label_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "inventory"."numbered_label";
DROP FUNCTION IF EXISTS wamn_history.inventory__numbered_label_log();
SELECT wamn_history.create_history_table('inventory', 'widget', false);
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "inventory"."widget";
CREATE OR REPLACE FUNCTION wamn_history.inventory__widget_stamp()
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
            OR NEW."code"::text IS DISTINCT FROM OLD."code"::text
            OR NEW."note"::text IS DISTINCT FROM OLD."note"::text
            OR NEW."maker_id"::text IS DISTINCT FROM OLD."maker_id"::text
            OR NEW."edit_version"::text IS DISTINCT FROM OLD."edit_version"::text) THEN
        NEW."created_at" := OLD."created_at";
    ELSE
        NEW."created_at" := OLD."created_at";
    END IF;
    RETURN NEW;
END
$stamp$;;
CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON inventory.widget FOR EACH ROW EXECUTE FUNCTION wamn_history.inventory__widget_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "inventory"."widget";
CREATE OR REPLACE FUNCTION wamn_history.inventory__widget_log()
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
            OR NEW."code"::text IS DISTINCT FROM OLD."code"::text
            OR NEW."note"::text IS DISTINCT FROM OLD."note"::text
            OR NEW."maker_id"::text IS DISTINCT FROM OLD."maker_id"::text
            OR NEW."edit_version"::text IS DISTINCT FROM OLD."edit_version"::text
            OR NEW."created_at"::text IS DISTINCT FROM OLD."created_at"::text) THEN
            RETURN NULL;
        END IF;
        IF to_jsonb(OLD."id") = to_jsonb(NEW."id") THEN
            IF NEW."id"::text IS DISTINCT FROM OLD."id"::text THEN
                changed_before := changed_before || jsonb_build_object('id', OLD."id");
                changed_after := changed_after || jsonb_build_object('id', NEW."id");
            END IF;
            IF NEW."code"::text IS DISTINCT FROM OLD."code"::text THEN
                changed_before := changed_before || jsonb_build_object('code', OLD."code");
                changed_after := changed_after || jsonb_build_object('code', NEW."code");
            END IF;
            IF NEW."note"::text IS DISTINCT FROM OLD."note"::text THEN
                changed_before := changed_before || jsonb_build_object('note', OLD."note");
                changed_after := changed_after || jsonb_build_object('note', NEW."note");
            END IF;
            IF NEW."maker_id"::text IS DISTINCT FROM OLD."maker_id"::text THEN
                changed_before := changed_before || jsonb_build_object('maker_id', OLD."maker_id");
                changed_after := changed_after || jsonb_build_object('maker_id', NEW."maker_id");
            END IF;
            IF NEW."edit_version"::text IS DISTINCT FROM OLD."edit_version"::text THEN
                changed_before := changed_before || jsonb_build_object('edit_version', OLD."edit_version");
                changed_after := changed_after || jsonb_build_object('edit_version', NEW."edit_version");
            END IF;
            IF NEW."created_at"::text IS DISTINCT FROM OLD."created_at"::text THEN
                changed_before := changed_before || jsonb_build_object('created_at', OLD."created_at");
                changed_after := changed_after || jsonb_build_object('created_at', NEW."created_at");
            END IF;
            INSERT INTO "inventory"."widget_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
            VALUES (jsonb_build_object('id', NEW."id"), 'update', operation, actor, transaction_timestamp(),
                    pg_current_xact_id()::text::bigint, changed_before, changed_after);
            RETURN NULL;
        END IF;
    END IF;
    IF TG_OP <> 'INSERT' THEN
        INSERT INTO "inventory"."widget_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', OLD."id"), 'delete', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, to_jsonb(OLD), '{}');
    END IF;
    IF TG_OP <> 'DELETE' THEN
        INSERT INTO "inventory"."widget_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', NEW."id"), 'insert', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, '{}', to_jsonb(NEW));
    END IF;
    RETURN NULL;
END
$log$;;
CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE ON inventory.widget FOR EACH ROW EXECUTE FUNCTION wamn_history.inventory__widget_log('P30D');
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "inventory"."widget_maker";
CREATE OR REPLACE FUNCTION wamn_history.inventory__widget_maker_stamp()
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
            OR NEW."name"::text IS DISTINCT FROM OLD."name"::text
            OR NEW."edit_version"::text IS DISTINCT FROM OLD."edit_version"::text) THEN
        NEW."created_at" := OLD."created_at";
    ELSE
        NEW."created_at" := OLD."created_at";
    END IF;
    RETURN NEW;
END
$stamp$;;
CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON inventory.widget_maker FOR EACH ROW EXECUTE FUNCTION wamn_history.inventory__widget_maker_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "inventory"."widget_maker";
DROP FUNCTION IF EXISTS wamn_history.inventory__widget_maker_log();
SELECT wamn_history.create_history_table('inventory', 'widget_tag', false);
DROP TRIGGER IF EXISTS wamn_record_history_stamp ON "inventory"."widget_tag";
DROP FUNCTION IF EXISTS wamn_history.inventory__widget_tag_stamp();
DROP TRIGGER IF EXISTS wamn_record_history_log ON "inventory"."widget_tag";
CREATE OR REPLACE FUNCTION wamn_history.inventory__widget_tag_log()
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
            OR NEW."label"::text IS DISTINCT FROM OLD."label"::text
            OR NEW."edit_version"::text IS DISTINCT FROM OLD."edit_version"::text) THEN
            RETURN NULL;
        END IF;
        IF to_jsonb(OLD."id") = to_jsonb(NEW."id") THEN
            IF NEW."id"::text IS DISTINCT FROM OLD."id"::text THEN
                changed_before := changed_before || jsonb_build_object('id', OLD."id");
                changed_after := changed_after || jsonb_build_object('id', NEW."id");
            END IF;
            IF NEW."label"::text IS DISTINCT FROM OLD."label"::text THEN
                changed_before := changed_before || jsonb_build_object('label', OLD."label");
                changed_after := changed_after || jsonb_build_object('label', NEW."label");
            END IF;
            IF NEW."edit_version"::text IS DISTINCT FROM OLD."edit_version"::text THEN
                changed_before := changed_before || jsonb_build_object('edit_version', OLD."edit_version");
                changed_after := changed_after || jsonb_build_object('edit_version', NEW."edit_version");
            END IF;
            INSERT INTO "inventory"."widget_tag_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
            VALUES (jsonb_build_object('id', NEW."id"), 'update', operation, actor, transaction_timestamp(),
                    pg_current_xact_id()::text::bigint, changed_before, changed_after);
            RETURN NULL;
        END IF;
    END IF;
    IF TG_OP <> 'INSERT' THEN
        INSERT INTO "inventory"."widget_tag_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', OLD."id"), 'delete', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, to_jsonb(OLD), '{}');
    END IF;
    IF TG_OP <> 'DELETE' THEN
        INSERT INTO "inventory"."widget_tag_history" (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES (jsonb_build_object('id', NEW."id"), 'insert', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, '{}', to_jsonb(NEW));
    END IF;
    RETURN NULL;
END
$log$;;
CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE ON inventory.widget_tag FOR EACH ROW EXECUTE FUNCTION wamn_history.inventory__widget_tag_log('P30D');
