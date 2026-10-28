-- The order row anchors the read, so one snapshot tells a missing order (no
-- row) from an order with no lines (one row with null line columns).
SELECT
    purchase_order.id AS purchase_order_id,
    purchase_order.purchase_order_number,
    purchase_order.status AS purchase_order_status,
    purchase_order.supplier_id,
    purchase_order.row_version,
    line.line_id,
    line.line_number,
    line.item_id,
    line.item_number,
    line.ordered_quantity,
    line.received_quantity,
    line.remaining_quantity
FROM purchase_order AS purchase_order
LEFT JOIN receipt_screen_line AS line
    ON line.purchase_order_id = purchase_order.id
WHERE purchase_order.id = $1::uuid
ORDER BY line.line_number ASC, line.line_id ASC;
