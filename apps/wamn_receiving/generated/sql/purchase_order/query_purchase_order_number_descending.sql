SELECT
    model.created_at,
    model.created_by,
    model.id,
    model.purchase_order_number,
    model.row_version,
    model.status,
    model.supplier_id,
    model.updated_at,
    model.updated_by
FROM purchase_order AS model
WHERE
    ($1::jsonb IS NULL OR model.supplier_id IN (
        SELECT filter.value::uuid
        FROM jsonb_array_elements_text($1::jsonb) AS filter(value)
    ))
    AND
    ($2::jsonb IS NULL OR model.status IN (
        SELECT filter.value::text
        FROM jsonb_array_elements_text($2::jsonb) AS filter(value)
    ))
    AND
    ($3::jsonb IS NULL OR EXISTS (
        SELECT 1
        FROM jsonb_array_elements_text($3::jsonb) AS filter(value)
        WHERE strpos(model.purchase_order_number, filter.value) > 0
    ))
    AND
    ($4::text IS NULL OR model.purchase_order_number < $4::text
        OR (model.purchase_order_number = $4::text AND model.id < $5::uuid))
ORDER BY model.purchase_order_number DESC, model.id DESC
LIMIT $6::int8;
