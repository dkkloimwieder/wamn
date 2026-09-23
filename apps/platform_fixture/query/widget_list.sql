SELECT id, code, edit_version, to_jsonb(widget) AS attributes FROM widget ORDER BY id;
