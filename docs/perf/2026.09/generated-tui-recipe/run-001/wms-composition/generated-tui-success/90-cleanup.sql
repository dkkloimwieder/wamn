BEGIN;
DELETE FROM wms.inventory_movement WHERE pallet_id = '680a1da3-7625-46ea-9880-64019aa2fd86';
DELETE FROM wms.inventory_move_command WHERE pallet_id = '680a1da3-7625-46ea-9880-64019aa2fd86';
DELETE FROM wms.pallet_quantity WHERE pallet_id = '680a1da3-7625-46ea-9880-64019aa2fd86';
DELETE FROM wms.pallet WHERE id = '680a1da3-7625-46ea-9880-64019aa2fd86';
DELETE FROM wms.location WHERE id IN ('50208ba6-65a8-4774-aba0-51aacfa108e2', 'fc4030f3-0473-4d61-8403-2014e34d1d64');
DELETE FROM wms.product WHERE id = 'fa998805-3cad-440a-a6e2-b7734844a2f1';
COMMIT;
SELECT (SELECT count(*) FROM wms.pallet WHERE id = '680a1da3-7625-46ea-9880-64019aa2fd86')
  + (SELECT count(*) FROM wms.location WHERE id IN ('50208ba6-65a8-4774-aba0-51aacfa108e2', 'fc4030f3-0473-4d61-8403-2014e34d1d64'))
  + (SELECT count(*) FROM wms.product WHERE id = 'fa998805-3cad-440a-a6e2-b7734844a2f1');
