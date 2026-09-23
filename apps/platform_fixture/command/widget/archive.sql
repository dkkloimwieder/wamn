SELECT id, edit_version, note FROM widget WHERE id = $1 FOR UPDATE;
