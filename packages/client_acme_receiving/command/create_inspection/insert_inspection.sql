INSERT INTO quality_inspection (receipt_id)
VALUES ($1)
ON CONFLICT ON CONSTRAINT quality_inspection_receipt_id_pkey
DO NOTHING
RETURNING receipt_id;
