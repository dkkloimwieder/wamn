UPDATE pallet
SET
    row_version = row_version + 1
WHERE id = $1
RETURNING row_version, status;
