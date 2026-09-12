BEGIN;
DELETE FROM receiving.receipt_line WHERE receipt_id IN
  (SELECT id FROM receiving.receipt WHERE purchase_order_id = 'ec359b6d-3db6-4e47-8b9a-711f1e451219');
DELETE FROM receiving.receipt WHERE purchase_order_id = 'ec359b6d-3db6-4e47-8b9a-711f1e451219';
DELETE FROM receiving.record_receipt_command WHERE purchase_order_id = 'ec359b6d-3db6-4e47-8b9a-711f1e451219';
DELETE FROM receiving.purchase_order_line WHERE purchase_order_id = 'ec359b6d-3db6-4e47-8b9a-711f1e451219';
DELETE FROM receiving.purchase_order WHERE id = 'ec359b6d-3db6-4e47-8b9a-711f1e451219';
DELETE FROM receiving.location WHERE id IN ('64be6eea-7bf1-4d13-8b34-10373c60fc39', 'a9dd51e7-c60e-4246-9bd2-0821e7ba6db7');
DELETE FROM receiving.item WHERE id = '55a80035-98f0-4f5d-8fc2-ab83d0964f86';
COMMIT;
SELECT (SELECT count(*) FROM receiving.purchase_order WHERE id = 'ec359b6d-3db6-4e47-8b9a-711f1e451219')
  + (SELECT count(*) FROM receiving.location WHERE id IN ('64be6eea-7bf1-4d13-8b34-10373c60fc39', 'a9dd51e7-c60e-4246-9bd2-0821e7ba6db7'))
  + (SELECT count(*) FROM receiving.item WHERE id = '55a80035-98f0-4f5d-8fc2-ab83d0964f86');
