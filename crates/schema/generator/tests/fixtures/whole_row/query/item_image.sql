-- to_jsonb(item) reads every column of item. The lexer attributes only id.
SELECT
    to_jsonb(item)::text AS image
FROM item AS item
ORDER BY item.id ASC;
