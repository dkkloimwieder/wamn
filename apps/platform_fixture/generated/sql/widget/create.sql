INSERT INTO widget (id, code, maker_id, note)
VALUES ($1::uuid, $2::text, $3::uuid, $4::text)
RETURNING
    code,
    created_at,
    edit_version,
    id,
    maker_id,
    note;
