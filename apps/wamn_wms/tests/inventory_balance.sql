-- Every balance equals the sum of its transactions: the stock that arrived on
-- a to side, less the stock that left on a from side. Each row returned is a
-- disagreement, so an empty result means that the balances and the
-- transactions agree.
WITH sided AS (
    SELECT to_packaging_id AS packaging_id, product_id, to_status AS status, quantity
    FROM wms.inventory_transaction
    WHERE to_packaging_id IS NOT NULL
    UNION ALL
    SELECT from_packaging_id, product_id, from_status, -quantity
    FROM wms.inventory_transaction
    WHERE from_packaging_id IS NOT NULL
),
summed AS (
    SELECT packaging_id, product_id, status, sum(quantity) AS quantity
    FROM sided
    GROUP BY packaging_id, product_id, status
)
SELECT
    coalesce(balance.packaging_id, summed.packaging_id)::text AS packaging_id,
    coalesce(balance.product_id, summed.product_id)::text AS product_id,
    coalesce(balance.status, summed.status) AS status,
    balance.quantity::text AS balance,
    summed.quantity::text AS transactions
FROM wms.packaging_quantity AS balance
FULL OUTER JOIN summed
    ON summed.packaging_id = balance.packaging_id
    AND summed.product_id = balance.product_id
    AND summed.status = balance.status
WHERE coalesce(balance.quantity, 0) <> coalesce(summed.quantity, 0);
