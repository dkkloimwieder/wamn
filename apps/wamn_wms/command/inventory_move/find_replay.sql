SELECT canonical_command, result, operation_id FROM inventory_move_command WHERE idempotency_key = $1;
