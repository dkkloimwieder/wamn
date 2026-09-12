BEGIN;
INSERT INTO receiving.item (id, item_number) VALUES
  ('816e65b6-0281-4a81-a512-f63848b316ee', 'PREFLIGHT-99544d16-ITEM');
INSERT INTO receiving.location (id, location_code) VALUES
  ('38ed7d93-e53b-4ffa-990e-2737bae8bc01', 'PREFLIGHT-99544d16-A'), ('17cf6a33-bce0-466d-ad74-0898a139370c', 'PREFLIGHT-99544d16-B');
INSERT INTO receiving.purchase_order
  (id, purchase_order_number, supplier_id, created_at, updated_at)
SELECT '5a102c0c-262f-485a-8d86-40cbbd6be6a4', 'PREFLIGHT-99544d16-PO', '46c7ebf3-eb65-4129-a2e2-b386d0ebb8be',
  COALESCE(MIN(created_at), CURRENT_TIMESTAMP) - interval '1 second', CURRENT_TIMESTAMP
FROM receiving.purchase_order;
INSERT INTO receiving.purchase_order_line
  (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity)
VALUES
  ('dc27d7cd-d5f9-4aa5-888e-e6366e5ce04f', '5a102c0c-262f-485a-8d86-40cbbd6be6a4', 1, '816e65b6-0281-4a81-a512-f63848b316ee', 5.0000, 0.0000),
  ('8847d63d-54ae-4874-bdfc-dcf701fa97f9', '5a102c0c-262f-485a-8d86-40cbbd6be6a4', 2, '816e65b6-0281-4a81-a512-f63848b316ee', 7.0000, 0.0000);
COMMIT;
