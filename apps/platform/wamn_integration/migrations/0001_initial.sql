CREATE TABLE wamn_integration.setting (
    key text CONSTRAINT setting_key_pkey PRIMARY KEY,
    value jsonb NOT NULL
);
CREATE TABLE wamn_integration.log (
    delivery_id text NOT NULL,
    record_id text NOT NULL,
    outcome text NOT NULL CONSTRAINT log_outcome_check CHECK (outcome IN ('pushed', 'refused')),
    recorded_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    detail jsonb,
    CONSTRAINT log_delivery_id_record_id_outcome_pkey PRIMARY KEY (delivery_id, record_id, outcome)
);
