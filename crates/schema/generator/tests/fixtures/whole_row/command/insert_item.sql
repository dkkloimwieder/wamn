-- RETURNING item reads every column of item. The lexer attributes no read.
INSERT INTO item (id, sku, note)
VALUES ($1::uuid, 'probe', 'probe')
RETURNING item;
