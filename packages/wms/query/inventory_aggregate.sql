-- LIVE STOCK ONLY. A consumed pallet keeps its quantity rows as history of
-- what it held when a merge absorbed it — the platform admits no DELETE — and
-- that same quantity now also sits on the target. Counting both would double
-- every merge this warehouse has ever done, so the join excludes them here,
-- at the one place that reports totals.
SELECT
    pallet_quantity.product_id,
    pallet.location_id,
    pallet_quantity.status,
    sum(pallet_quantity.quantity) AS quantity,
    count(*) AS pallet_count
FROM pallet_quantity AS pallet_quantity
JOIN pallet AS pallet
    ON pallet.id = pallet_quantity.pallet_id
WHERE pallet.status <> 'consumed'
GROUP BY
    pallet_quantity.product_id,
    pallet.location_id,
    pallet_quantity.status
ORDER BY
    pallet_quantity.product_id ASC,
    pallet.location_id ASC,
    pallet_quantity.status ASC;
