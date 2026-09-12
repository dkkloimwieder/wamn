BEGIN;
DELETE FROM wms.inventory_movement WHERE pallet_id = '58b5d7f5-f024-4abc-a468-94bc0f7dc754';
DELETE FROM wms.inventory_move_command WHERE pallet_id = '58b5d7f5-f024-4abc-a468-94bc0f7dc754';
DELETE FROM wms.pallet_quantity WHERE pallet_id = '58b5d7f5-f024-4abc-a468-94bc0f7dc754';
DELETE FROM wms.pallet WHERE id = '58b5d7f5-f024-4abc-a468-94bc0f7dc754';
DELETE FROM wms.location WHERE id IN ('85ac7f2c-dfae-4a56-93a4-914872137d29', '66a47e8a-464a-43f3-984b-df9b9012ccca');
DELETE FROM wms.product WHERE id = '529565cf-cffa-42e5-8efd-87cf0f9d3274';
COMMIT;
SELECT (SELECT count(*) FROM wms.pallet WHERE id = '58b5d7f5-f024-4abc-a468-94bc0f7dc754')
  + (SELECT count(*) FROM wms.location WHERE id IN ('85ac7f2c-dfae-4a56-93a4-914872137d29', '66a47e8a-464a-43f3-984b-df9b9012ccca'))
  + (SELECT count(*) FROM wms.product WHERE id = '529565cf-cffa-42e5-8efd-87cf0f9d3274');
