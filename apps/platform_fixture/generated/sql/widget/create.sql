INSERT INTO widget (code, maker_id, note)
VALUES ($1::text, $2::uuid, $3::text)
RETURNING
    code,
    created_at,
    edit_version,
    id,
    maker_id,
    note;
