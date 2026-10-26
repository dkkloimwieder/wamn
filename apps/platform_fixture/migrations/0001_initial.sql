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
    edit_version int4 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT widget_code_check
        CHECK (code IN ('priority', 'standard'))
);

CREATE TABLE inventory.widget_tag (
    id uuid CONSTRAINT widget_tag_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    label text NOT NULL,
    edit_version int8 NOT NULL DEFAULT 1
);

CREATE TABLE inventory.numbered_label (
    id uuid CONSTRAINT numbered_label_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    number text NOT NULL CONSTRAINT numbered_label_number_key UNIQUE,
    stock_id text NOT NULL,
    edit_version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_by uuid NOT NULL
);
