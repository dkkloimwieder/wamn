CREATE TABLE inventory.counter (
    id bigint GENERATED ALWAYS AS IDENTITY
        CONSTRAINT counter_id_pkey PRIMARY KEY,
    label text NOT NULL,
    edit_version int8 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);
