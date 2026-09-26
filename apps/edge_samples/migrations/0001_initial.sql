CREATE TABLE edge_samples.sample (
    id uuid CONSTRAINT sample_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    frame text NOT NULL,
    captured_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE edge_samples.sample_command (
    idempotency_key text
        CONSTRAINT sample_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL,
    sample_id uuid NOT NULL DEFAULT gen_random_uuid()
        CONSTRAINT sample_command_sample_id_key UNIQUE,
    CONSTRAINT sample_command_canonical_command_check
        CHECK (octet_length(canonical_command) > 0)
);
