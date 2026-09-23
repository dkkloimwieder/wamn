INSERT INTO widget_command (canonical_command, idempotency_key) VALUES ($1, $2) ON CONFLICT DO NOTHING RETURNING widget_id;
