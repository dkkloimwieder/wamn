SELECT canonical_command, result, operation_id, new_inventory_id FROM inventory_split_command WHERE idempotency_key = $1;
