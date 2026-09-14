-- One page of the history of one stock item, in ascending position order.
-- Every row repeats the current row image and the head position, so the
-- fold reads one snapshot. A deleted row has the current image '{}'.
-- A page holds at most 100 entries.
SELECT
    history.position,
    history.kind,
    history.operation,
    history.changed_by,
    history.changed_at,
    history.transaction_id,
    history.before::text AS before,
    history.after::text AS after,
    COALESCE(
        (SELECT wamn_history.row_image(stock_item)::text
           FROM stock_item AS stock_item
          WHERE stock_item.id = $1::uuid),
        '{}'
    ) AS current,
    (SELECT max(head.position)
       FROM stock_item_history AS head
      WHERE head.row_key = jsonb_build_object('id', $1::uuid)) AS head_position
FROM stock_item_history AS history
WHERE history.row_key = jsonb_build_object('id', $1::uuid)
  AND history.position > $2::bigint
ORDER BY history.position ASC
LIMIT LEAST($3::bigint, 100);
