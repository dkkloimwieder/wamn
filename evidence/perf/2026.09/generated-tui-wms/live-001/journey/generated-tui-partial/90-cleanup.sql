BEGIN;
DELETE FROM wms.inventory_movement WHERE pallet_id = 'd6beedfc-6826-4ecc-9040-b392cb9e8a54';
DELETE FROM wms.inventory_move_command WHERE pallet_id = 'd6beedfc-6826-4ecc-9040-b392cb9e8a54';
DELETE FROM wms.pallet_quantity WHERE pallet_id = 'd6beedfc-6826-4ecc-9040-b392cb9e8a54';
DELETE FROM wms.pallet WHERE id = 'd6beedfc-6826-4ecc-9040-b392cb9e8a54';
DELETE FROM wms.location WHERE id IN ('5835bc13-daf7-432d-b029-b3e5cc9ede8a', '83be6ceb-0494-4f7a-908a-8f01cb7f0bf9');
DELETE FROM wms.product WHERE id = 'b96b38f1-1fd2-413b-b131-594c15472008';
COMMIT;
SELECT (SELECT count(*) FROM wms.pallet WHERE id = 'd6beedfc-6826-4ecc-9040-b392cb9e8a54')
  + (SELECT count(*) FROM wms.location WHERE id IN ('5835bc13-daf7-432d-b029-b3e5cc9ede8a', '83be6ceb-0494-4f7a-908a-8f01cb7f0bf9'))
  + (SELECT count(*) FROM wms.product WHERE id = 'b96b38f1-1fd2-413b-b131-594c15472008');
