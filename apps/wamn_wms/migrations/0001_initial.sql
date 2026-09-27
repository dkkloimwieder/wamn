CREATE TABLE wms.product (
    id uuid CONSTRAINT product_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    product_code text NOT NULL CONSTRAINT product_product_code_key UNIQUE,
    row_version int4 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE wms.location (
    id uuid CONSTRAINT location_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    location_code text NOT NULL CONSTRAINT location_location_code_key UNIQUE,
    row_version int4 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE wms.pallet (
    id uuid CONSTRAINT pallet_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    pallet_code text NOT NULL CONSTRAINT pallet_pallet_code_key UNIQUE,
    location_id uuid NOT NULL
        CONSTRAINT pallet_location_id_fkey
        REFERENCES wms.location (id),
    status text NOT NULL,
    row_version int4 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
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
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT pallet_quantity_pallet_id_product_id_status_key
        UNIQUE (pallet_id, product_id, status),
    CONSTRAINT pallet_quantity_status_check
        CHECK (status IN ('available', 'held')),
    CONSTRAINT pallet_quantity_quantity_check CHECK (quantity > 0)
);

CREATE TABLE wms.inventory_movement (
    id uuid CONSTRAINT inventory_movement_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
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
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
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
