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

CREATE TABLE wms.inventory_movement (
    id uuid CONSTRAINT inventory_movement_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    idempotency_key text NOT NULL
        CONSTRAINT inventory_movement_idempotency_key_key UNIQUE
        CONSTRAINT inventory_movement_idempotency_key_fkey
        REFERENCES wms.inventory_move_command (idempotency_key),
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
