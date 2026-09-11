SELECT
    purchase_order.id AS purchase_order_id,
    purchase_order.purchase_order_number,
    purchase_order.status AS purchase_order_status,
    purchase_order.supplier_id,
    purchase_order.row_version,
    purchase_order_line.id AS line_id,
    purchase_order_line.line_number,
    purchase_order_line.item_id,
    item.item_number,
    purchase_order_line.ordered_quantity,
    purchase_order_line.received_quantity,
    purchase_order_line.ordered_quantity - purchase_order_line.received_quantity
        AS remaining_quantity
FROM purchase_order AS purchase_order
LEFT JOIN purchase_order_line AS purchase_order_line
    ON purchase_order_line.purchase_order_id = purchase_order.id
LEFT JOIN item AS item
    ON item.id = purchase_order_line.item_id
WHERE purchase_order.id = $1::uuid
ORDER BY
    purchase_order_line.line_number ASC,
    purchase_order_line.id ASC;
