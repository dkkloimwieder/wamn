BEGIN;
INSERT INTO receiving.item (id, item_number) VALUES
  ('55a80035-98f0-4f5d-8fc2-ab83d0964f86', 'TUI-90e470f4-ITEM');
INSERT INTO receiving.location (id, location_code) VALUES
  ('64be6eea-7bf1-4d13-8b34-10373c60fc39', 'TUI-90e470f4-A'), ('a9dd51e7-c60e-4246-9bd2-0821e7ba6db7', 'TUI-90e470f4-B');
INSERT INTO receiving.purchase_order
  (id, purchase_order_number, supplier_id, created_at, updated_at)
SELECT 'ec359b6d-3db6-4e47-8b9a-711f1e451219', 'TUI-90e470f4-PO', '10c7ca6e-111f-4b77-8a4b-5577811699d2',
  COALESCE(MIN(created_at), CURRENT_TIMESTAMP) - interval '1 second', CURRENT_TIMESTAMP
FROM receiving.purchase_order;
INSERT INTO receiving.purchase_order_line
  (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity)
VALUES
  ('1a84e539-5c43-40ef-8cf0-7cea3bb61eab', 'ec359b6d-3db6-4e47-8b9a-711f1e451219', 1, '55a80035-98f0-4f5d-8fc2-ab83d0964f86', 5.0000, 0.0000),
  ('79b02819-6583-4a2f-9a0d-19d73c5e5057', 'ec359b6d-3db6-4e47-8b9a-711f1e451219', 2, '55a80035-98f0-4f5d-8fc2-ab83d0964f86', 7.0000, 0.0000);
COMMIT;
