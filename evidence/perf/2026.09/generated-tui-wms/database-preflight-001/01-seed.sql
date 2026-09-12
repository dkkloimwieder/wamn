BEGIN;
INSERT INTO wms.product (id, product_code) VALUES ('6feb3b12-2b4b-4380-ab4d-90a17f3a79b4', 'PREFLIGHT-9e15c819-PRODUCT');
INSERT INTO wms.location (id, location_code) VALUES
  ('66207bf6-a214-4f52-8513-ece62e83c0ab', 'PREFLIGHT-9e15c819-FROM'), ('d4156737-6292-40ca-91f1-c4a0d6b7d0c1', 'PREFLIGHT-9e15c819-TO');
INSERT INTO wms.pallet (id, pallet_code, location_id, status)
  VALUES ('1dfee0b3-0c5c-4171-be99-34fa533ea0cd', 'PREFLIGHT-9e15c819-PALLET', '66207bf6-a214-4f52-8513-ece62e83c0ab', 'available');
INSERT INTO wms.pallet_quantity (pallet_id, product_id, quantity, status)
  VALUES ('1dfee0b3-0c5c-4171-be99-34fa533ea0cd', '6feb3b12-2b4b-4380-ab4d-90a17f3a79b4', 10.0000, 'available');
COMMIT;
