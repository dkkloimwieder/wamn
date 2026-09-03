-- BY id, ALWAYS. Two merges naming the same pair in opposite orders would
-- deadlock if each locked its own source first; ordering by a total order the
-- database shares makes that impossible rather than unlikely.
SELECT
    id,
    location_id,
    row_version,
    status
FROM pallet
WHERE id IN ($1, $2)
ORDER BY id ASC
FOR UPDATE;
