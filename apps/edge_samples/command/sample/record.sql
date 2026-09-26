INSERT INTO sample (id, frame, captured_at) VALUES ($1, $2, $3) RETURNING id;
