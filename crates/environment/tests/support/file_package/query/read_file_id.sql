SELECT file_id AS document
FROM document
WHERE id = $1::uuid
