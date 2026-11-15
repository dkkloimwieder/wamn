INSERT INTO log (delivery_id, record_id, outcome, detail) VALUES ($1, $2, $3, $4)
ON CONFLICT ON CONSTRAINT log_delivery_id_record_id_outcome_pkey DO NOTHING RETURNING delivery_id, record_id, outcome;
