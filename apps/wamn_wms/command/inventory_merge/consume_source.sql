-- The source holds no stock after a merge: its balance rows are deleted, and
-- the packaging stays as a consumed record that a transaction row can name.
UPDATE packaging
SET
    status = 'consumed',
    row_version = row_version + 1
WHERE id = $1
RETURNING row_version;
