INSERT INTO item (key, name, external_system_id, external_id) VALUES ($1, $2, $3, $1)
ON CONFLICT (external_system_id, external_id) DO NOTHING
RETURNING key, external_system_id, external_id;
