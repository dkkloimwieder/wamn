ALTER TABLE receiving.purchase_order ADD CONSTRAINT purchase_order_supplier_id_excl EXCLUDE USING gist (supplier_id WITH =);
