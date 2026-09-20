INSERT INTO quality_inspection (receipt_id)
SELECT receipt.id
FROM receipt
JOIN purchase_order ON purchase_order.id = receipt.purchase_order_id
WHERE receipt.id = $1
  AND purchase_order.acme_inspection_required IS TRUE
ON CONFLICT ON CONSTRAINT quality_inspection_receipt_id_pkey
DO NOTHING
RETURNING receipt_id;
