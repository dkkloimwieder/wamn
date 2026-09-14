-- This statement names each column that it reads, so the derived grants cover it.
SELECT
    item.id,
    item.sku
FROM item AS item
ORDER BY item.id ASC;
