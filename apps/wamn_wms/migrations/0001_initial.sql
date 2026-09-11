CREATE TABLE wms.product (
    id uuid CONSTRAINT product_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    product_code text NOT NULL CONSTRAINT product_product_code_key UNIQUE
);

CREATE TABLE wms.location (
    id uuid CONSTRAINT location_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    location_code text NOT NULL CONSTRAINT location_location_code_key UNIQUE
);

CREATE TABLE wms.pallet (
    id uuid CONSTRAINT pallet_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    pallet_code text NOT NULL CONSTRAINT pallet_pallet_code_key UNIQUE,
    location_id uuid NOT NULL
        CONSTRAINT pallet_location_id_fkey
        REFERENCES wms.location (id),
    status text NOT NULL,
    row_version int8 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT pallet_status_check
        CHECK (status IN ('available', 'held', 'consumed'))
);

CREATE TABLE wms.pallet_quantity (
    id uuid CONSTRAINT pallet_quantity_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    pallet_id uuid NOT NULL
        CONSTRAINT pallet_quantity_pallet_id_fkey
        REFERENCES wms.pallet (id),
    product_id uuid NOT NULL
        CONSTRAINT pallet_quantity_product_id_fkey
        REFERENCES wms.product (id),
    status text NOT NULL,
    quantity numeric NOT NULL,
    CONSTRAINT pallet_quantity_pallet_id_product_id_status_key
        UNIQUE (pallet_id, product_id, status),
    CONSTRAINT pallet_quantity_status_check
        CHECK (status IN ('available', 'held')),
    CONSTRAINT pallet_quantity_quantity_check CHECK (quantity > 0)
);

CREATE TABLE wms.inventory_move_command (
    idempotency_key text
        CONSTRAINT inventory_move_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL,
    movement_id uuid NOT NULL DEFAULT gen_random_uuid()
        CONSTRAINT inventory_move_command_movement_id_key UNIQUE,
    pallet_id uuid NOT NULL,
    pallet_status text,
    row_version int8,
    CONSTRAINT inventory_move_command_canonical_command_check
        CHECK (octet_length(canonical_command) > 0),
    CONSTRAINT inventory_move_command_pallet_status_check
        CHECK (
            pallet_status IS NULL
            OR pallet_status IN ('available', 'held', 'consumed')
        ),
    CONSTRAINT inventory_move_command_row_version_check
        CHECK (row_version IS NULL OR row_version > 0),
    CONSTRAINT inventory_move_command_pallet_status_row_version_check
        CHECK (
            (pallet_status IS NULL AND row_version IS NULL)
            OR (pallet_status IS NOT NULL AND row_version IS NOT NULL)
        )
);

CREATE TABLE wms.inventory_adjust_command (
    idempotency_key text
        CONSTRAINT inventory_adjust_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL,
    movement_id uuid NOT NULL DEFAULT gen_random_uuid()
        CONSTRAINT inventory_adjust_command_movement_id_key UNIQUE,
    pallet_id uuid NOT NULL,
    adjusted_quantity numeric,
    row_version int8,
    CONSTRAINT inventory_adjust_command_canonical_command_check
        CHECK (octet_length(canonical_command) > 0),
    CONSTRAINT inventory_adjust_command_adjusted_quantity_check
        CHECK (adjusted_quantity IS NULL OR adjusted_quantity > 0),
    CONSTRAINT inventory_adjust_command_row_version_check
        CHECK (row_version IS NULL OR row_version > 0),
    CONSTRAINT inventory_adjust_command_adjusted_quantity_row_version_check
        CHECK (
            (adjusted_quantity IS NULL AND row_version IS NULL)
            OR (adjusted_quantity IS NOT NULL AND row_version IS NOT NULL)
        )
);

CREATE TABLE wms.inventory_merge_command (
    idempotency_key text
        CONSTRAINT inventory_merge_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL,
    movement_id uuid NOT NULL DEFAULT gen_random_uuid()
        CONSTRAINT inventory_merge_command_movement_id_key UNIQUE,
    source_pallet_id uuid NOT NULL,
    target_pallet_id uuid NOT NULL,
    row_version int8,
    CONSTRAINT inventory_merge_command_canonical_command_check
        CHECK (octet_length(canonical_command) > 0),
    CONSTRAINT inventory_merge_command_source_pallet_id_target_pallet_id_check
        CHECK (source_pallet_id <> target_pallet_id),
    CONSTRAINT inventory_merge_command_row_version_check
        CHECK (row_version IS NULL OR row_version > 0)
);

CREATE TABLE wms.inventory_split_command (
    idempotency_key text
        CONSTRAINT inventory_split_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL,
    movement_id uuid NOT NULL DEFAULT gen_random_uuid()
        CONSTRAINT inventory_split_command_movement_id_key UNIQUE,
    source_pallet_id uuid NOT NULL,
    new_pallet_id uuid NOT NULL DEFAULT gen_random_uuid()
        CONSTRAINT inventory_split_command_new_pallet_id_key UNIQUE,
    row_version int8,
    CONSTRAINT inventory_split_command_canonical_command_check
        CHECK (octet_length(canonical_command) > 0),
    CONSTRAINT inventory_split_command_row_version_check
        CHECK (row_version IS NULL OR row_version > 0)
);

CREATE TABLE wms.inventory_movement (
    id uuid CONSTRAINT inventory_movement_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    -- No foreign key: FOUR commands write movements, so this cannot reference
    -- one claim table. Each command's own claim row is the authority for its
    -- existence, and this column groups the rows one command wrote.
    idempotency_key text NOT NULL,
    pallet_id uuid NOT NULL
        CONSTRAINT inventory_movement_pallet_id_fkey
        REFERENCES wms.pallet (id),
    product_id uuid NOT NULL
        CONSTRAINT inventory_movement_product_id_fkey
        REFERENCES wms.product (id),
    kind text NOT NULL,
    from_location_id uuid
        CONSTRAINT inventory_movement_from_location_id_fkey
        REFERENCES wms.location (id),
    to_location_id uuid
        CONSTRAINT inventory_movement_to_location_id_fkey
        REFERENCES wms.location (id),
    quantity numeric NOT NULL,
    reason_code text,
    occurred_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT inventory_movement_kind_check
        CHECK (kind IN ('move', 'adjust', 'merge', 'split')),
    CONSTRAINT inventory_movement_quantity_check CHECK (quantity > 0),
    CONSTRAINT inventory_movement_kind_from_location_id_to_location_id_check
        CHECK (
            kind <> 'move'
            OR (
                from_location_id IS NOT NULL
                AND to_location_id IS NOT NULL
                AND from_location_id <> to_location_id
            )
        ),
    CONSTRAINT inventory_movement_kind_reason_code_check
        CHECK (kind <> 'adjust' OR reason_code IS NOT NULL)
);
