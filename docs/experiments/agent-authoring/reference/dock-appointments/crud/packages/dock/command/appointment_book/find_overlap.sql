SELECT id
FROM appointment
WHERE dock_id = $1
    AND slot_start < $3
    AND slot_end > $2
ORDER BY slot_start
LIMIT 1;
