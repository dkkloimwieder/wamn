INSERT INTO inventory_merge_command (idempotency_key, canonical_command) VALUES ($1, $2) ON CONFLICT (idempotency_key) DO NOTHING RETURNING operation_id;
