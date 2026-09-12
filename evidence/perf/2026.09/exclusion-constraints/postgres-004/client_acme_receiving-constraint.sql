ALTER TABLE receiving.purchase_order ADD CONSTRAINT purchase_order_acme_quality_status_excl EXCLUDE USING gist (acme_quality_status WITH =);
