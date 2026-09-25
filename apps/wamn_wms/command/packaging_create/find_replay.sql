SELECT canonical_command, result, operation_id, packaging_id FROM packaging_create_command WHERE idempotency_key = $1;
