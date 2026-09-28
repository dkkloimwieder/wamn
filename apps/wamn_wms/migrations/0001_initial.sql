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

CREATE TABLE wms.packaging (
    id uuid CONSTRAINT packaging_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    packaging_code text NOT NULL CONSTRAINT packaging_packaging_code_key UNIQUE,
    location_id uuid NOT NULL
        CONSTRAINT packaging_location_id_fkey
        REFERENCES wms.location (id),
    located_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    type text NOT NULL,
    status text NOT NULL,
    row_version int4 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    CONSTRAINT packaging_type_check
        CHECK (type IN ('pallet', 'tote', 'bin', 'case', 'loose')),
    CONSTRAINT packaging_status_check
        CHECK (status IN ('available', 'held', 'consumed'))
);

CREATE TABLE wms.packaging_quantity (
    id uuid CONSTRAINT packaging_quantity_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    packaging_id uuid NOT NULL
        CONSTRAINT packaging_quantity_packaging_id_fkey
        REFERENCES wms.packaging (id),
    product_id uuid NOT NULL
        CONSTRAINT packaging_quantity_product_id_fkey
        REFERENCES wms.product (id),
    status text NOT NULL,
    quantity numeric NOT NULL,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT packaging_quantity_packaging_id_product_id_status_key
        UNIQUE (packaging_id, product_id, status),
    CONSTRAINT packaging_quantity_status_check
        CHECK (status IN ('available', 'held')),
    CONSTRAINT packaging_quantity_quantity_check CHECK (quantity > 0)
);

CREATE TABLE wms.inventory_transaction (
    id uuid CONSTRAINT inventory_transaction_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id uuid NOT NULL
        CONSTRAINT inventory_transaction_product_id_fkey
        REFERENCES wms.product (id),
    quantity numeric NOT NULL,
    from_packaging_id uuid
        CONSTRAINT inventory_transaction_from_packaging_id_fkey
        REFERENCES wms.packaging (id),
    from_status text,
    to_packaging_id uuid
        CONSTRAINT inventory_transaction_to_packaging_id_fkey
        REFERENCES wms.packaging (id),
    to_status text,
    occurred_at timestamptz NOT NULL,
    reason_code text,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    CONSTRAINT inventory_transaction_quantity_check CHECK (quantity > 0),
    CONSTRAINT inventory_transaction_from_status_check
        CHECK (from_status IN ('available', 'held')),
    CONSTRAINT inventory_transaction_to_status_check
        CHECK (to_status IN ('available', 'held')),
    CONSTRAINT inventory_transaction_from_packaging_id_from_status_check
        CHECK ((from_packaging_id IS NULL) = (from_status IS NULL)),
    CONSTRAINT inventory_transaction_to_packaging_id_to_status_check
        CHECK ((to_packaging_id IS NULL) = (to_status IS NULL)),
    CONSTRAINT inventory_transaction_from_packaging_id_to_packaging_id_check
        CHECK (
            from_packaging_id IS NOT NULL
            OR to_packaging_id IS NOT NULL
        ),
    CONSTRAINT inventory_transaction_sides_check
        CHECK (
            from_packaging_id IS DISTINCT FROM to_packaging_id
            OR from_status IS DISTINCT FROM to_status
        )
);
