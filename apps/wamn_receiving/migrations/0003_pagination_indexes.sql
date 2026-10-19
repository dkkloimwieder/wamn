CREATE INDEX purchase_order_id_created_at_idx ON receiving.purchase_order (created_at, id);
CREATE INDEX purchase_order_id_purchase_order_number_idx ON receiving.purchase_order (purchase_order_number, id);
CREATE INDEX purchase_order_id_status_idx ON receiving.purchase_order (status, id);
CREATE INDEX receipt_id_created_at_idx ON receiving.receipt (created_at, id);
CREATE INDEX supplier_id_created_at_idx ON receiving.supplier (created_at, id);
