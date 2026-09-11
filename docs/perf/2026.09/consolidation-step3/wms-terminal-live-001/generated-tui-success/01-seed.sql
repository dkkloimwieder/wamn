BEGIN;
INSERT INTO wms.product (id, product_code) VALUES ('33d4e068-366d-4032-99af-a7677d2e4f60', 'WMS-TUI-299e2c2f-PRODUCT');
INSERT INTO wms.location (id, location_code) VALUES
  ('50d880eb-9634-4b60-a7ad-1ad68c437303', 'WMS-TUI-299e2c2f-FROM'), ('bb0c153e-ce37-4f32-8ed8-af44c1d29f84', 'WMS-TUI-299e2c2f-TO');
INSERT INTO wms.pallet (id, pallet_code, location_id, status)
  VALUES ('8bc0d39c-62f4-4de2-a170-5947643544dc', 'WMS-TUI-299e2c2f-PALLET', '50d880eb-9634-4b60-a7ad-1ad68c437303', 'available');
INSERT INTO wms.pallet_quantity (pallet_id, product_id, quantity, status)
  VALUES ('8bc0d39c-62f4-4de2-a170-5947643544dc', '33d4e068-366d-4032-99af-a7677d2e4f60', 10.0000, 'available');
COMMIT;
