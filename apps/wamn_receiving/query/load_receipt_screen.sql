SELECT
    purchase_order_id,
    purchase_order_number,
    purchase_order_status,
    supplier_id,
    purchase_order_row_version,
    line_id,
    line_number,
    item_id,
    item_number,
    ordered_quantity,
    received_quantity,
    remaining_quantity
FROM receipt_screen_line
WHERE purchase_order_id = $1::uuid
ORDER BY line_number ASC, line_id ASC;
