SELECT
    id,
    carrier_id,
    dock_id,
    slot_start,
    slot_end,
    status,
    arrived_at
FROM appointment
WHERE dock_id = $1
    AND slot_start >= $2
    AND slot_start < $3
    AND status = $4
ORDER BY slot_start ASC, id ASC;
