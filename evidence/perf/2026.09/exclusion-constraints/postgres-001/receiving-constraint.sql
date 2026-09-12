ALTER TABLE receiving.purchase_order ADD CONSTRAINT purchase_order_allowed_exclusion EXCLUDE USING gist (supplier_id WITH =);
