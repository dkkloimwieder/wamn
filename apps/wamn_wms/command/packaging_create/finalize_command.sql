UPDATE packaging_create_command SET result = $4 WHERE idempotency_key = $1 AND canonical_command = $2 AND operation_id = $3 AND result IS NULL RETURNING result;
