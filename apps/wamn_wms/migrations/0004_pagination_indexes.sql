CREATE INDEX inventory_transaction_id_created_at_idx ON wms.inventory_transaction (created_at, id);
CREATE INDEX location_id_created_at_idx ON wms.location (created_at, id);
CREATE INDEX packaging_id_created_at_idx ON wms.packaging (created_at, id);
CREATE INDEX packaging_id_location_id_idx ON wms.packaging (location_id, id);
CREATE INDEX packaging_id_packaging_code_idx ON wms.packaging (packaging_code, id);
CREATE INDEX packaging_id_updated_at_idx ON wms.packaging (updated_at, id);
CREATE INDEX packaging_quantity_id_created_at_idx ON wms.packaging_quantity (created_at, id);
CREATE INDEX product_id_created_at_idx ON wms.product (created_at, id);
