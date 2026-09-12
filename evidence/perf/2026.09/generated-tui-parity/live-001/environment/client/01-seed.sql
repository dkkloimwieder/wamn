BEGIN;
INSERT INTO receiving.item (id, item_number) VALUES
  ('9861742a-4f79-43e3-9dde-633e512a8fcd', 'TUI-932e6323-ITEM');
INSERT INTO receiving.location (id, location_code) VALUES
  ('43919903-c433-4a2f-9a79-ac3a3c29ee42', 'TUI-932e6323-A'), ('f6e476de-6939-443c-a447-f98b517a9cbe', 'TUI-932e6323-B');
INSERT INTO receiving.purchase_order
  (id, purchase_order_number, supplier_id, created_at, updated_at)
SELECT '2065e759-7932-43c4-aca4-eb6eb93013da', 'TUI-932e6323-PO', 'a961a22c-19c7-40ee-9451-4a69b68af86c',
  COALESCE(MIN(created_at), CURRENT_TIMESTAMP) - interval '1 second', CURRENT_TIMESTAMP
FROM receiving.purchase_order;
INSERT INTO receiving.purchase_order_line
  (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity)
VALUES
  ('cdd4ac98-55cf-43f9-a473-a764f13c663b', '2065e759-7932-43c4-aca4-eb6eb93013da', 1, '9861742a-4f79-43e3-9dde-633e512a8fcd', 5.0000, 0.0000),
  ('014e59e4-b26a-4102-b58c-db169fcec04c', '2065e759-7932-43c4-aca4-eb6eb93013da', 2, '9861742a-4f79-43e3-9dde-633e512a8fcd', 7.0000, 0.0000);
COMMIT;
