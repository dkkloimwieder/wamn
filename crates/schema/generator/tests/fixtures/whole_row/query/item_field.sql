-- (item).sku reads the whole row of item. The lexer attributes only id.
SELECT
    (item).sku
FROM item AS item
ORDER BY item.id ASC;
