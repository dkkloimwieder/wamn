BEGIN;
DELETE FROM wms.inventory_movement WHERE pallet_id = '8bc0d39c-62f4-4de2-a170-5947643544dc';
DELETE FROM wms.inventory_move_command WHERE pallet_id = '8bc0d39c-62f4-4de2-a170-5947643544dc';
DELETE FROM wms.pallet_quantity WHERE pallet_id = '8bc0d39c-62f4-4de2-a170-5947643544dc';
DELETE FROM wms.pallet WHERE id = '8bc0d39c-62f4-4de2-a170-5947643544dc';
DELETE FROM wms.location WHERE id IN ('50d880eb-9634-4b60-a7ad-1ad68c437303', 'bb0c153e-ce37-4f32-8ed8-af44c1d29f84');
DELETE FROM wms.product WHERE id = '33d4e068-366d-4032-99af-a7677d2e4f60';
COMMIT;
SELECT (SELECT count(*) FROM wms.pallet WHERE id = '8bc0d39c-62f4-4de2-a170-5947643544dc')
  + (SELECT count(*) FROM wms.location WHERE id IN ('50d880eb-9634-4b60-a7ad-1ad68c437303', 'bb0c153e-ce37-4f32-8ed8-af44c1d29f84'))
  + (SELECT count(*) FROM wms.product WHERE id = '33d4e068-366d-4032-99af-a7677d2e4f60');
