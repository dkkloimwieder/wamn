SELECT id, status
FROM appointment
WHERE id = $1
FOR UPDATE;
