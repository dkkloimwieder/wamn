SET search_path = receiving, public;
INSERT INTO receiving.purchase_order(id,purchase_order_number,supplier_id,acme_quality_status) VALUES ('10000000-0000-0000-0000-000000000001','proof-1','20000000-0000-0000-0000-000000000001','pending'), ('10000000-0000-0000-0000-000000000002','proof-2','20000000-0000-0000-0000-000000000002','approved');
PREPARE exclusion_update AS WITH target AS MATERIALIZED (
    SELECT id, row_version
    FROM purchase_order
    WHERE id = $1::uuid
    FOR UPDATE
),
updated AS (
    UPDATE purchase_order AS model
    SET
        acme_inspection_required = CASE WHEN $3::boolean THEN $4::boolean ELSE model.acme_inspection_required END,
        acme_quality_status = CASE WHEN $5::boolean THEN $6::text ELSE model.acme_quality_status END,
        row_version = model.row_version + 1
    FROM target
    WHERE model.id = target.id
      AND target.row_version = $2::int8
    RETURNING model.*
)
SELECT
    CASE
        WHEN NOT EXISTS (SELECT 1 FROM target) THEN 'not_found'
        WHEN NOT EXISTS (SELECT 1 FROM updated) THEN 'concurrency_conflict'
        ELSE 'updated'
    END AS outcome,
    (SELECT target.row_version FROM target) AS observed_row_version,
    updated.acme_inspection_required,
    updated.acme_quality_status,
    updated.created_at,
    updated.id,
    updated.purchase_order_number,
    updated.row_version,
    updated.status,
    updated.supplier_id,
    updated.updated_at
FROM (SELECT 1) AS singleton
LEFT JOIN updated ON TRUE;

CREATE FUNCTION pg_temp.refusal() RETURNS json LANGUAGE plpgsql AS $proof$
DECLARE state text; constraint_name text; message text;
BEGIN
    EXECUTE $command$EXECUTE exclusion_update('10000000-0000-0000-0000-000000000002',1,false,NULL,true,'pending')$command$;
    RAISE EXCEPTION 'generated update did not violate the exclusion';
EXCEPTION WHEN exclusion_violation THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE,
        constraint_name = CONSTRAINT_NAME, message = MESSAGE_TEXT;
    RETURN json_build_object('sqlstate', state, 'constraint', constraint_name, 'message', message);
END
$proof$;
SELECT pg_temp.refusal();
