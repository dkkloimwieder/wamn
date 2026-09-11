BEGIN;
INSERT INTO wms.product (id, product_code) VALUES ('5e081c7f-cd74-4a64-b5e9-eb33b2c31b0b', 'WMS-TUI-3df9f92e-PRODUCT');
INSERT INTO wms.location (id, location_code) VALUES
  ('c779bf81-f464-4d6f-962a-12d2fb131be3', 'WMS-TUI-3df9f92e-FROM'), ('b5d4872a-aeef-41e2-8a40-0bcf5e6fea6e', 'WMS-TUI-3df9f92e-TO');
INSERT INTO wms.pallet (id, pallet_code, location_id, status)
  VALUES ('fe91468a-d49b-4809-8ff4-de580ed4028d', 'WMS-TUI-3df9f92e-PALLET', 'c779bf81-f464-4d6f-962a-12d2fb131be3', 'available');
INSERT INTO wms.pallet_quantity (pallet_id, product_id, quantity, status)
  VALUES ('fe91468a-d49b-4809-8ff4-de580ed4028d', '5e081c7f-cd74-4a64-b5e9-eb33b2c31b0b', 10.0000, 'available');
COMMIT;
