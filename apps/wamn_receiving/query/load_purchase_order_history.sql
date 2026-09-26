-- One page of the history of one purchase order, in ascending position order.
-- Every row repeats the current row image, so the fold reads one snapshot.
-- A deleted row has the current image '{}'.
-- The current image names each declared purchase_order column, so the read
-- needs no grant on a column that an overlay adds. It spells each timestamptz
-- column through the platform image function, like the entry images.
-- A page holds at most 100 entries. The position stays inside this read: the
-- operation answers with an opaque cursor, and a short page is the last page.
SELECT
    history.id,
    history.position,
    history.kind,
    history.operation,
    history.changed_by,
    history.changed_at,
    history.before::text AS before,
    history.after::text AS after,
    COALESCE(
        (SELECT jsonb_build_object(
                    'id', purchase_order.id,
                    'purchase_order_number', purchase_order.purchase_order_number,
                    'supplier_id', purchase_order.supplier_id,
                    'status', purchase_order.status,
                    'row_version', purchase_order.row_version,
                    'created_at', wamn_history.timestamptz_image(purchase_order.created_at),
                    'created_by', purchase_order.created_by,
                    'updated_at', wamn_history.timestamptz_image(purchase_order.updated_at),
                    'updated_by', purchase_order.updated_by
                )::text
           FROM purchase_order AS purchase_order
          WHERE purchase_order.id = $1::uuid),
        '{}'
    ) AS current
FROM purchase_order_history AS history
WHERE history.row_key = jsonb_build_object('id', $1::uuid)
  AND history.position > $2::bigint
ORDER BY history.position ASC
LIMIT LEAST($3::int4, 100);
