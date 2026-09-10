SELECT json_build_object(
  'claims', (SELECT count(*) FROM wms.inventory_move_command WHERE pallet_id = '680a1da3-7625-46ea-9880-64019aa2fd86'),
  'movements', (SELECT count(*) FROM wms.inventory_movement WHERE pallet_id = '680a1da3-7625-46ea-9880-64019aa2fd86'),
  'command', (SELECT row_to_json(command) FROM (
    SELECT idempotency_key, movement_id, pallet_id, pallet_status, row_version
    FROM wms.inventory_move_command WHERE pallet_id = '680a1da3-7625-46ea-9880-64019aa2fd86') AS command),
  'movement', (SELECT row_to_json(movement) FROM (
    SELECT idempotency_key, pallet_id, product_id, from_location_id, to_location_id, kind, quantity::text
    FROM wms.inventory_movement WHERE pallet_id = '680a1da3-7625-46ea-9880-64019aa2fd86') AS movement),
  'pallet', (SELECT json_build_object('location_id', location_id, 'status', status, 'row_version', row_version)
    FROM wms.pallet WHERE id = '680a1da3-7625-46ea-9880-64019aa2fd86'),
  'quantity', (SELECT json_agg(json_build_object('product_id', product_id, 'quantity', quantity::text, 'status', status))
    FROM wms.pallet_quantity WHERE pallet_id = '680a1da3-7625-46ea-9880-64019aa2fd86'));
