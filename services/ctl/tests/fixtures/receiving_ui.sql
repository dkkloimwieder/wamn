-- Business rows only. The Rust fixture binds the actor and tenant.
INSERT INTO receiving.item (id, item_number)
VALUES ('00000000-0000-0000-0000-000000000101', 'ITEM-BROWSER');
INSERT INTO receiving.location (id, location_code)
VALUES ('00000000-0000-0000-0000-000000000201', 'DOCK-BROWSER');
INSERT INTO receiving.supplier (id, name)
VALUES ('00000000-0000-0000-0000-000000000401', 'SUPPLIER-BROWSER');
WITH orders AS (
    INSERT INTO receiving.purchase_order (purchase_order_number, supplier_id, status)
    SELECT 'PO-BROWSER-' || lpad(n::text, 2, '0'),
           '00000000-0000-0000-0000-000000000401'::uuid, 'open'
    FROM generate_series(1, 26) AS n
    RETURNING id
)
INSERT INTO receiving.purchase_order_line
    (purchase_order_id, line_number, item_id, ordered_quantity, received_quantity)
SELECT id, 1, '00000000-0000-0000-0000-000000000101'::uuid, 5, 0
FROM orders;
