BEGIN;
INSERT INTO wms.product (id, product_code) VALUES ('34544be2-08a5-45e8-98a6-b7f85da83048', 'WMS-TUI-8bdc322f-PRODUCT');
INSERT INTO wms.location (id, location_code) VALUES
  ('e0922dc9-9f2e-43ec-9f48-58a2821bda8a', 'WMS-TUI-8bdc322f-FROM'), ('a49807da-b9b9-46e1-b8ea-341d05940f9d', 'WMS-TUI-8bdc322f-TO');
INSERT INTO wms.pallet (id, pallet_code, location_id, status)
  VALUES ('a9ece470-ad01-427c-8cb6-5f255b192b56', 'WMS-TUI-8bdc322f-PALLET', 'e0922dc9-9f2e-43ec-9f48-58a2821bda8a', 'available');
INSERT INTO wms.pallet_quantity (pallet_id, product_id, quantity, status)
  VALUES ('a9ece470-ad01-427c-8cb6-5f255b192b56', '34544be2-08a5-45e8-98a6-b7f85da83048', 10.0000, 'available');
COMMIT;
