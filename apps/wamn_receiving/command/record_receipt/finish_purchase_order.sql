WITH final_state AS (
    SELECT bool_and(received_quantity = ordered_quantity) AS complete
    FROM purchase_order_line
    WHERE purchase_order_id = $1
)
UPDATE purchase_order
SET
    status = CASE
        WHEN coalesce(final_state.complete, false) THEN 'complete'
        ELSE 'open'
    END,
    row_version = purchase_order.row_version + 1,
    updated_at = CURRENT_TIMESTAMP
FROM final_state
WHERE purchase_order.id = $1
    AND purchase_order.status = 'open'
RETURNING purchase_order.status, purchase_order.row_version;
