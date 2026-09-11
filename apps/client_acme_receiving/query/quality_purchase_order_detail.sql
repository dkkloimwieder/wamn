SELECT
    purchase_order.id,
    purchase_order.purchase_order_number,
    purchase_order.supplier_id,
    purchase_order.status,
    purchase_order.row_version,
    purchase_order.acme_inspection_required,
    purchase_order.acme_quality_status
FROM purchase_order
WHERE purchase_order.id = $1;
