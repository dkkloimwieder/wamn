BEGIN;
INSERT INTO wms.product (id, product_code) VALUES ('b96b38f1-1fd2-413b-b131-594c15472008', 'WMS-TUI-bb87d1ac-PRODUCT');
INSERT INTO wms.location (id, location_code) VALUES
  ('5835bc13-daf7-432d-b029-b3e5cc9ede8a', 'WMS-TUI-bb87d1ac-FROM'), ('83be6ceb-0494-4f7a-908a-8f01cb7f0bf9', 'WMS-TUI-bb87d1ac-TO');
INSERT INTO wms.pallet (id, pallet_code, location_id, status)
  VALUES ('d6beedfc-6826-4ecc-9040-b392cb9e8a54', 'WMS-TUI-bb87d1ac-PALLET', '5835bc13-daf7-432d-b029-b3e5cc9ede8a', 'available');
INSERT INTO wms.pallet_quantity (pallet_id, product_id, quantity, status)
  VALUES ('d6beedfc-6826-4ecc-9040-b392cb9e8a54', 'b96b38f1-1fd2-413b-b131-594c15472008', 10.0000, 'available');
COMMIT;
