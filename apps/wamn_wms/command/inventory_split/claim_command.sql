INSERT INTO inventory_split_command (idempotency_key, canonical_command) VALUES ($1, $2) ON CONFLICT (idempotency_key) DO NOTHING RETURNING operation_id, new_inventory_id;
