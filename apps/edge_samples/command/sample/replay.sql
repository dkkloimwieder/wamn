SELECT canonical_command, sample_id FROM sample_command WHERE idempotency_key = $1;
