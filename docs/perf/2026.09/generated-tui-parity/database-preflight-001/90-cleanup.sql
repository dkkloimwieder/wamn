BEGIN;
DELETE FROM receiving.receipt_line WHERE receipt_id IN
  (SELECT id FROM receiving.receipt WHERE purchase_order_id = '5a102c0c-262f-485a-8d86-40cbbd6be6a4');
DELETE FROM receiving.receipt WHERE purchase_order_id = '5a102c0c-262f-485a-8d86-40cbbd6be6a4';
DELETE FROM receiving.record_receipt_command WHERE purchase_order_id = '5a102c0c-262f-485a-8d86-40cbbd6be6a4';
DELETE FROM receiving.purchase_order_line WHERE purchase_order_id = '5a102c0c-262f-485a-8d86-40cbbd6be6a4';
DELETE FROM receiving.purchase_order WHERE id = '5a102c0c-262f-485a-8d86-40cbbd6be6a4';
DELETE FROM receiving.location WHERE id IN ('38ed7d93-e53b-4ffa-990e-2737bae8bc01', '17cf6a33-bce0-466d-ad74-0898a139370c');
DELETE FROM receiving.item WHERE id = '816e65b6-0281-4a81-a512-f63848b316ee';
COMMIT;
SELECT (SELECT count(*) FROM receiving.purchase_order WHERE id = '5a102c0c-262f-485a-8d86-40cbbd6be6a4')
  + (SELECT count(*) FROM receiving.location WHERE id IN ('38ed7d93-e53b-4ffa-990e-2737bae8bc01', '17cf6a33-bce0-466d-ad74-0898a139370c'))
  + (SELECT count(*) FROM receiving.item WHERE id = '816e65b6-0281-4a81-a512-f63848b316ee');
