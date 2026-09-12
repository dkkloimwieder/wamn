SELECT
    model.acme_inspection_required,
    model.acme_quality_status,
    model.created_at,
    model.id,
    model.purchase_order_number,
    model.row_version,
    model.status,
    model.supplier_id,
    model.updated_at
FROM purchase_order AS model
WHERE model.id = $1::uuid;
