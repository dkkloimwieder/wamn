SELECT canonical_command, result, operation_id FROM inventory_adjust_command WHERE idempotency_key = $1;
