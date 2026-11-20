SELECT
    purchase_order_id AS "purchase_order_id!",
    purchase_order_number AS "purchase_order_number!",
    purchase_order_status AS "purchase_order_status!",
    supplier_id AS "supplier_id!",
    purchase_order_row_version AS "purchase_order_row_version!",
    line_id AS "line_id!",
    line_number AS "line_number!",
    item_id AS "item_id!",
    item_number AS "item_number!",
    ordered_quantity AS "ordered_quantity!",
    received_quantity AS "received_quantity!",
    remaining_quantity AS "remaining_quantity!"
FROM receipt_screen_line
WHERE purchase_order_status = 'open'
ORDER BY purchase_order_id ASC, line_number ASC, line_id ASC;
