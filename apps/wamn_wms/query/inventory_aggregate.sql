-- LIVE STOCK ONLY. A merge deletes the balance rows of the packaging it
-- consumes, and the filter keeps that rule at the one place that reports
-- totals.
SELECT
    packaging_quantity.product_id,
    packaging.location_id,
    packaging_quantity.status,
    sum(packaging_quantity.quantity) AS quantity,
    count(*)::integer AS packaging_count
FROM packaging_quantity AS packaging_quantity
JOIN packaging AS packaging
    ON packaging.id = packaging_quantity.packaging_id
WHERE packaging.status <> 'consumed'
GROUP BY
    packaging_quantity.product_id,
    packaging.location_id,
    packaging_quantity.status
ORDER BY
    packaging_quantity.product_id ASC,
    packaging.location_id ASC,
    packaging_quantity.status ASC;
