INSERT INTO appointment (
    id,
    carrier_id,
    dock_id,
    slot_start,
    slot_end
)
VALUES ($1, $2, $3, $4, $5)
RETURNING id, status;
