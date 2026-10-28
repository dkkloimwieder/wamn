DROP VIEW receiving.receipt_screen_line;

CREATE VIEW receiving.receipt_screen_line AS
SELECT
    line.id AS line_id,
    line.purchase_order_id,
    purchase_order.purchase_order_number,
    purchase_order.status AS purchase_order_status,
    purchase_order.supplier_id,
    purchase_order.row_version AS purchase_order_row_version,
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

CREATE INDEX purchase_order_line_item_id_idx ON receiving.purchase_order_line (item_id);
