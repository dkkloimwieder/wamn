BEGIN;
DELETE FROM wms.inventory_movement WHERE pallet_id = 'fe91468a-d49b-4809-8ff4-de580ed4028d';
DELETE FROM wms.inventory_move_command WHERE pallet_id = 'fe91468a-d49b-4809-8ff4-de580ed4028d';
DELETE FROM wms.pallet_quantity WHERE pallet_id = 'fe91468a-d49b-4809-8ff4-de580ed4028d';
DELETE FROM wms.pallet WHERE id = 'fe91468a-d49b-4809-8ff4-de580ed4028d';
DELETE FROM wms.location WHERE id IN ('c779bf81-f464-4d6f-962a-12d2fb131be3', 'b5d4872a-aeef-41e2-8a40-0bcf5e6fea6e');
DELETE FROM wms.product WHERE id = '5e081c7f-cd74-4a64-b5e9-eb33b2c31b0b';
COMMIT;
SELECT (SELECT count(*) FROM wms.pallet WHERE id = 'fe91468a-d49b-4809-8ff4-de580ed4028d')
  + (SELECT count(*) FROM wms.location WHERE id IN ('c779bf81-f464-4d6f-962a-12d2fb131be3', 'b5d4872a-aeef-41e2-8a40-0bcf5e6fea6e'))
  + (SELECT count(*) FROM wms.product WHERE id = '5e081c7f-cd74-4a64-b5e9-eb33b2c31b0b');
