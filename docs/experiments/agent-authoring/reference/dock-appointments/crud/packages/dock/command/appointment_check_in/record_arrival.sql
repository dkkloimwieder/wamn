UPDATE appointment
SET
    status = 'arrived',
    arrived_at = $2
WHERE id = $1
    AND status = 'scheduled'
RETURNING id, status, arrived_at;
