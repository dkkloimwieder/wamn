SELECT
    purchase_order_id AS "purchase_order_id!",
    purchase_order_number AS "purchase_order_number!",
    status AS "status!",
    supplier_id AS "supplier_id!",
    supplier_name AS "supplier_name!",
    line_count AS "line_count!",
    ordered_quantity AS "ordered_quantity!",
    received_quantity AS "received_quantity!"
FROM purchase_order_receiving_summary
WHERE supplier_id = $1::uuid AND status = $2::text
ORDER BY purchase_order_number ASC, purchase_order_id ASC;
