BEGIN;
INSERT INTO receiving.item (id, item_number) VALUES
  ('9845a478-1079-4654-af62-b0b58b3336c6', 'TUI-b3dee30f-ITEM');
INSERT INTO receiving.location (id, location_code) VALUES
  ('bee00daa-de4f-4f4c-85f0-3610414fe820', 'TUI-b3dee30f-A'), ('5e9a3e3c-5c62-4f89-b01d-c0b44b54eced', 'TUI-b3dee30f-B');
INSERT INTO receiving.purchase_order
  (id, purchase_order_number, supplier_id, created_at, updated_at)
SELECT '6e0d0c1e-352d-4259-95d4-95ee5b379e9a', 'TUI-b3dee30f-PO', 'cfeafc22-4670-4169-bc02-99bce8e35b89',
  COALESCE(MIN(created_at), CURRENT_TIMESTAMP) - interval '1 second', CURRENT_TIMESTAMP
FROM receiving.purchase_order;
INSERT INTO receiving.purchase_order_line
  (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity)
VALUES
  ('94ccbe99-05b7-405e-b9d4-bb8cbd7f8692', '6e0d0c1e-352d-4259-95d4-95ee5b379e9a', 1, '9845a478-1079-4654-af62-b0b58b3336c6', 5.0000, 0.0000),
  ('b8e7ab3c-d168-469a-a628-cb521bf18a2a', '6e0d0c1e-352d-4259-95d4-95ee5b379e9a', 2, '9845a478-1079-4654-af62-b0b58b3336c6', 7.0000, 0.0000);
COMMIT;
