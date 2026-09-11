-- TOMBSTONE, not deletion. The platform refuses DELETE in command SQL
-- (sql_lex.rs refuse_unsupported_effects), so a merge does not remove the
-- source pallet or its quantity rows: it marks the pallet consumed and leaves
-- the rows as history — what this pallet held when it was absorbed.
--
-- The consequence every reader must honour: LIVE STOCK EXCLUDES CONSUMED
-- PALLETS. A query that counted them would double-count everything a merge
-- moved, because the quantity now exists on the target as well.
UPDATE pallet
SET
    status = 'consumed',
    row_version = row_version + 1,
    updated_at = CURRENT_TIMESTAMP
WHERE id = $1
RETURNING row_version;
