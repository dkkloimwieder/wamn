INSERT INTO setting (key, value) VALUES ($1, $2)
ON CONFLICT (key) DO NOTHING RETURNING key;
