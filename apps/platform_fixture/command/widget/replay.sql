SELECT canonical_command, widget_id FROM widget_command WHERE idempotency_key = $1;
