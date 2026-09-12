BEGIN;
DELETE FROM receiving.receipt_line WHERE receipt_id IN
  (SELECT id FROM receiving.receipt WHERE purchase_order_id = '6e0d0c1e-352d-4259-95d4-95ee5b379e9a');
DELETE FROM receiving.receipt WHERE purchase_order_id = '6e0d0c1e-352d-4259-95d4-95ee5b379e9a';
DELETE FROM receiving.record_receipt_command WHERE purchase_order_id = '6e0d0c1e-352d-4259-95d4-95ee5b379e9a';
DELETE FROM receiving.purchase_order_line WHERE purchase_order_id = '6e0d0c1e-352d-4259-95d4-95ee5b379e9a';
DELETE FROM receiving.purchase_order WHERE id = '6e0d0c1e-352d-4259-95d4-95ee5b379e9a';
DELETE FROM receiving.location WHERE id IN ('bee00daa-de4f-4f4c-85f0-3610414fe820', '5e9a3e3c-5c62-4f89-b01d-c0b44b54eced');
DELETE FROM receiving.item WHERE id = '9845a478-1079-4654-af62-b0b58b3336c6';
COMMIT;
SELECT (SELECT count(*) FROM receiving.purchase_order WHERE id = '6e0d0c1e-352d-4259-95d4-95ee5b379e9a')
  + (SELECT count(*) FROM receiving.location WHERE id IN ('bee00daa-de4f-4f4c-85f0-3610414fe820', '5e9a3e3c-5c62-4f89-b01d-c0b44b54eced'))
  + (SELECT count(*) FROM receiving.item WHERE id = '9845a478-1079-4654-af62-b0b58b3336c6');
