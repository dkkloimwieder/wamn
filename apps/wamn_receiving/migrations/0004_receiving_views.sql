CREATE VIEW receiving.receipt_screen_line AS
SELECT
    line.id AS line_id,
    line.purchase_order_id,
    purchase_order.purchase_order_number,
    purchase_order.status AS purchase_order_status,
    purchase_order.supplier_id,
    purchase_order.row_version,
    line.line_number,
    line.item_id,
    item.item_number,
    line.ordered_quantity,
    line.received_quantity,
    line.ordered_quantity - line.received_quantity AS remaining_quantity
FROM receiving.purchase_order_line AS line
JOIN receiving.purchase_order AS purchase_order
    ON purchase_order.id = line.purchase_order_id
JOIN receiving.item AS item
    ON item.id = line.item_id;

CREATE VIEW receiving.purchase_order_receiving_summary AS
SELECT
    purchase_order.id AS purchase_order_id,
    purchase_order.purchase_order_number,
    purchase_order.status,
    purchase_order.supplier_id,
    supplier.name AS supplier_name,
    count(line.id) AS line_count,
    coalesce(sum(line.ordered_quantity), 0) AS ordered_quantity,
    coalesce(sum(line.received_quantity), 0) AS received_quantity
FROM receiving.purchase_order AS purchase_order
JOIN receiving.supplier AS supplier
    ON supplier.id = purchase_order.supplier_id
LEFT JOIN receiving.purchase_order_line AS line
    ON line.purchase_order_id = purchase_order.id
GROUP BY purchase_order.id, supplier.name;
