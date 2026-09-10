BEGIN;
INSERT INTO wms.product (id, product_code) VALUES ('fa998805-3cad-440a-a6e2-b7734844a2f1', 'WMS-TUI-a84ddc8f-PRODUCT');
INSERT INTO wms.location (id, location_code) VALUES
  ('50208ba6-65a8-4774-aba0-51aacfa108e2', 'WMS-TUI-a84ddc8f-FROM'), ('fc4030f3-0473-4d61-8403-2014e34d1d64', 'WMS-TUI-a84ddc8f-TO');
INSERT INTO wms.pallet (id, pallet_code, location_id, status)
  VALUES ('680a1da3-7625-46ea-9880-64019aa2fd86', 'WMS-TUI-a84ddc8f-PALLET', '50208ba6-65a8-4774-aba0-51aacfa108e2', 'available');
INSERT INTO wms.pallet_quantity (pallet_id, product_id, quantity, status)
  VALUES ('680a1da3-7625-46ea-9880-64019aa2fd86', 'fa998805-3cad-440a-a6e2-b7734844a2f1', 10.0000, 'available');
COMMIT;
