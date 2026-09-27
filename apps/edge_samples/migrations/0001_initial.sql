CREATE TABLE edge_samples.sample (
    id uuid CONSTRAINT sample_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    frame text NOT NULL,
    captured_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);
