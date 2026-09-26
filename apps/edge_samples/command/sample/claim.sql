INSERT INTO sample_command (canonical_command, idempotency_key) VALUES ($1, $2) ON CONFLICT DO NOTHING RETURNING sample_id;
