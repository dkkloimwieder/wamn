WITH purchase AS (
    SELECT acme_inspection_required, acme_quality_status
    FROM purchase_order
    WHERE id = $2
    FOR UPDATE
), inserted AS (
    INSERT INTO quality_inspection (receipt_id, status)
    SELECT $1, 'approved'
    FROM purchase
    WHERE acme_inspection_required IS TRUE
      AND acme_quality_status = 'approved'
    ON CONFLICT ON CONSTRAINT quality_inspection_receipt_id_pkey
    DO NOTHING
    RETURNING receipt_id
)
SELECT CASE
           WHEN acme_inspection_required IS NOT TRUE THEN 'not_required'
           WHEN acme_quality_status = 'approved' THEN 'approved'
           ELSE 'quality_not_approved'
       END AS outcome,
       (SELECT receipt_id FROM inserted) AS receipt_id
FROM purchase;
