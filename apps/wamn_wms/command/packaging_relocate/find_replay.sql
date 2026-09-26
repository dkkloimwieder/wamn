SELECT canonical_command, result, operation_id FROM packaging_relocate_command WHERE idempotency_key = $1;
