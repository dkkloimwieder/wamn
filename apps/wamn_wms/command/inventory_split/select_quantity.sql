-- The quantity row the split takes from, read under the pallet lock so the
-- refusal can say WHICH of two things is wrong: no such row at all
-- (quantity_not_found) or a row that cannot spare what was asked
-- (insufficient_quantity, with what it holds). take_from_source alone cannot
-- tell them apart -- it answers nothing in both cases.
SELECT quantity
FROM pallet_quantity
WHERE pallet_id = $1
    AND product_id = $2
    AND status = $3;
