BEGIN;
DELETE FROM wms.inventory_movement WHERE pallet_id = '1dfee0b3-0c5c-4171-be99-34fa533ea0cd';
DELETE FROM wms.inventory_move_command WHERE pallet_id = '1dfee0b3-0c5c-4171-be99-34fa533ea0cd';
DELETE FROM wms.pallet_quantity WHERE pallet_id = '1dfee0b3-0c5c-4171-be99-34fa533ea0cd';
DELETE FROM wms.pallet WHERE id = '1dfee0b3-0c5c-4171-be99-34fa533ea0cd';
DELETE FROM wms.location WHERE id IN ('66207bf6-a214-4f52-8513-ece62e83c0ab', 'd4156737-6292-40ca-91f1-c4a0d6b7d0c1');
DELETE FROM wms.product WHERE id = '6feb3b12-2b4b-4380-ab4d-90a17f3a79b4';
COMMIT;
SELECT (SELECT count(*) FROM wms.pallet WHERE id = '1dfee0b3-0c5c-4171-be99-34fa533ea0cd')
  + (SELECT count(*) FROM wms.location WHERE id IN ('66207bf6-a214-4f52-8513-ece62e83c0ab', 'd4156737-6292-40ca-91f1-c4a0d6b7d0c1'))
  + (SELECT count(*) FROM wms.product WHERE id = '6feb3b12-2b4b-4380-ab4d-90a17f3a79b4');
