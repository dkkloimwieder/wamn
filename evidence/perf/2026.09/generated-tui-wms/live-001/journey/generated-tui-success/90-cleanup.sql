BEGIN;
DELETE FROM wms.inventory_movement WHERE pallet_id = 'a9ece470-ad01-427c-8cb6-5f255b192b56';
DELETE FROM wms.inventory_move_command WHERE pallet_id = 'a9ece470-ad01-427c-8cb6-5f255b192b56';
DELETE FROM wms.pallet_quantity WHERE pallet_id = 'a9ece470-ad01-427c-8cb6-5f255b192b56';
DELETE FROM wms.pallet WHERE id = 'a9ece470-ad01-427c-8cb6-5f255b192b56';
DELETE FROM wms.location WHERE id IN ('e0922dc9-9f2e-43ec-9f48-58a2821bda8a', 'a49807da-b9b9-46e1-b8ea-341d05940f9d');
DELETE FROM wms.product WHERE id = '34544be2-08a5-45e8-98a6-b7f85da83048';
COMMIT;
SELECT (SELECT count(*) FROM wms.pallet WHERE id = 'a9ece470-ad01-427c-8cb6-5f255b192b56')
  + (SELECT count(*) FROM wms.location WHERE id IN ('e0922dc9-9f2e-43ec-9f48-58a2821bda8a', 'a49807da-b9b9-46e1-b8ea-341d05940f9d'))
  + (SELECT count(*) FROM wms.product WHERE id = '34544be2-08a5-45e8-98a6-b7f85da83048');
