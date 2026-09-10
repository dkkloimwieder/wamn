BEGIN;
INSERT INTO wms.product (id, product_code) VALUES ('529565cf-cffa-42e5-8efd-87cf0f9d3274', 'WMS-TUI-e9746663-PRODUCT');
INSERT INTO wms.location (id, location_code) VALUES
  ('85ac7f2c-dfae-4a56-93a4-914872137d29', 'WMS-TUI-e9746663-FROM'), ('66a47e8a-464a-43f3-984b-df9b9012ccca', 'WMS-TUI-e9746663-TO');
INSERT INTO wms.pallet (id, pallet_code, location_id, status)
  VALUES ('58b5d7f5-f024-4abc-a468-94bc0f7dc754', 'WMS-TUI-e9746663-PALLET', '85ac7f2c-dfae-4a56-93a4-914872137d29', 'available');
INSERT INTO wms.pallet_quantity (pallet_id, product_id, quantity, status)
  VALUES ('58b5d7f5-f024-4abc-a468-94bc0f7dc754', '529565cf-cffa-42e5-8efd-87cf0f9d3274', 10.0000, 'available');
COMMIT;
