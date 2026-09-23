CREATE TABLE inventory.ingot (
    id uuid CONSTRAINT ingot_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    ingot_number text NOT NULL CONSTRAINT ingot_ingot_number_key UNIQUE
);

CREATE TABLE inventory.dock (
    id uuid CONSTRAINT dock_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    dock_code text NOT NULL CONSTRAINT dock_dock_code_key UNIQUE
);

CREATE TABLE inventory.panel (
    id uuid CONSTRAINT panel_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    panel_number text NOT NULL
        CONSTRAINT panel_panel_number_key UNIQUE,
    stock_id uuid NOT NULL,
    status text NOT NULL DEFAULT 'open',
    row_version int8 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    CONSTRAINT panel_status_check
        CHECK (status IN ('open', 'complete', 'cancelled'))
);

CREATE TABLE inventory.panel_line (
    id uuid CONSTRAINT panel_line_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    panel_id uuid NOT NULL
        CONSTRAINT panel_line_panel_id_fkey
        REFERENCES inventory.panel (id),
    line_number int4 NOT NULL,
    ingot_id uuid NOT NULL
        CONSTRAINT panel_line_ingot_id_fkey
        REFERENCES inventory.ingot (id),
    ordered_quantity numeric NOT NULL,
    received_quantity numeric NOT NULL DEFAULT 0,
    CONSTRAINT panel_line_panel_id_line_number_key
        UNIQUE (panel_id, line_number),
    CONSTRAINT panel_line_ordered_quantity_check
        CHECK (ordered_quantity > 0),
    CONSTRAINT panel_line_ordered_quantity_received_quantity_check
        CHECK (
            received_quantity >= 0
            AND received_quantity <= ordered_quantity
        )
);

CREATE TABLE inventory.record_rack_command (
    idempotency_key text
        CONSTRAINT record_rack_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL,
    rack_id uuid NOT NULL DEFAULT gen_random_uuid()
        CONSTRAINT record_rack_command_rack_id_key UNIQUE,
    panel_id uuid NOT NULL,
    panel_status text,
    row_version int8,
    CONSTRAINT record_rack_command_canonical_command_check
        CHECK (octet_length(canonical_command) > 0),
    CONSTRAINT record_rack_command_panel_status_check
        CHECK (
            panel_status IS NULL
            OR panel_status IN ('open', 'complete')
        ),
    CONSTRAINT record_rack_command_row_version_check
        CHECK (row_version IS NULL OR row_version > 0),
    CONSTRAINT record_rack_command_panel_status_row_version_check
        CHECK (
            (panel_status IS NULL AND row_version IS NULL)
            OR (panel_status IS NOT NULL AND row_version IS NOT NULL)
        )
);

CREATE TABLE inventory.rack (
    id uuid CONSTRAINT rack_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    idempotency_key text NOT NULL
        CONSTRAINT rack_idempotency_key_key UNIQUE
        CONSTRAINT rack_idempotency_key_fkey
        REFERENCES inventory.record_rack_command (idempotency_key),
    panel_id uuid NOT NULL
        CONSTRAINT rack_panel_id_fkey
        REFERENCES inventory.panel (id),
    rack_reference text NOT NULL,
    occurred_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    CONSTRAINT rack_panel_id_rack_reference_key
        UNIQUE (panel_id, rack_reference)
);

CREATE TABLE inventory.rack_line (
    id uuid CONSTRAINT rack_line_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    rack_id uuid NOT NULL
        CONSTRAINT rack_line_rack_id_fkey
        REFERENCES inventory.rack (id),
    panel_line_id uuid NOT NULL
        CONSTRAINT rack_line_panel_line_id_fkey
        REFERENCES inventory.panel_line (id),
    quantity numeric NOT NULL,
    dock_id uuid NOT NULL
        CONSTRAINT rack_line_dock_id_fkey
        REFERENCES inventory.dock (id),
    CONSTRAINT rack_line_quantity_check CHECK (quantity > 0)
);
