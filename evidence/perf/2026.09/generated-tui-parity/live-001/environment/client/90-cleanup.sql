BEGIN;
DELETE FROM receiving.receipt_line WHERE receipt_id IN
  (SELECT id FROM receiving.receipt WHERE purchase_order_id = '2065e759-7932-43c4-aca4-eb6eb93013da');
DELETE FROM receiving.receipt WHERE purchase_order_id = '2065e759-7932-43c4-aca4-eb6eb93013da';
DELETE FROM receiving.record_receipt_command WHERE purchase_order_id = '2065e759-7932-43c4-aca4-eb6eb93013da';
DELETE FROM receiving.purchase_order_line WHERE purchase_order_id = '2065e759-7932-43c4-aca4-eb6eb93013da';
DELETE FROM receiving.purchase_order WHERE id = '2065e759-7932-43c4-aca4-eb6eb93013da';
DELETE FROM receiving.location WHERE id IN ('43919903-c433-4a2f-9a79-ac3a3c29ee42', 'f6e476de-6939-443c-a447-f98b517a9cbe');
DELETE FROM receiving.item WHERE id = '9861742a-4f79-43e3-9dde-633e512a8fcd';
COMMIT;
SELECT (SELECT count(*) FROM receiving.purchase_order WHERE id = '2065e759-7932-43c4-aca4-eb6eb93013da')
  + (SELECT count(*) FROM receiving.location WHERE id IN ('43919903-c433-4a2f-9a79-ac3a3c29ee42', 'f6e476de-6939-443c-a447-f98b517a9cbe'))
  + (SELECT count(*) FROM receiving.item WHERE id = '9861742a-4f79-43e3-9dde-633e512a8fcd');
