CREATE TABLE inventory.widget_maker (
    id uuid CONSTRAINT widget_maker_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    name text NOT NULL,
    edit_version int8 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE inventory.widget (
    id uuid CONSTRAINT widget_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    code text NOT NULL CONSTRAINT widget_code_key UNIQUE,
    note text,
    maker_id uuid
        CONSTRAINT widget_maker_id_fkey
        REFERENCES inventory.widget_maker (id),
    edit_version int8 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT widget_code_check
        CHECK (code IN ('priority', 'standard'))
);

CREATE TABLE inventory.widget_command (
    canonical_command bytea NOT NULL,
    idempotency_key text NOT NULL CONSTRAINT widget_command_idempotency_key_pkey PRIMARY KEY,
    widget_id uuid NOT NULL DEFAULT gen_random_uuid()
        CONSTRAINT widget_command_widget_id_key UNIQUE
);

CREATE TABLE inventory.widget_tag (
    id uuid CONSTRAINT widget_tag_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    label text NOT NULL,
    edit_version int8 NOT NULL DEFAULT 1
);
