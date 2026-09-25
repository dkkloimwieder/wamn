CREATE TABLE wms.product (
    id uuid CONSTRAINT product_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    product_code text NOT NULL CONSTRAINT product_product_code_key UNIQUE,
    row_version int4 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE wms.product_command (
    idempotency_key text
        CONSTRAINT product_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL,
    product_id uuid NOT NULL DEFAULT gen_random_uuid()
        CONSTRAINT product_command_product_id_key UNIQUE,
    CONSTRAINT product_command_canonical_command_check
        CHECK (octet_length(canonical_command) > 0)
);

CREATE TABLE wms.location (
    id uuid CONSTRAINT location_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    location_code text NOT NULL CONSTRAINT location_location_code_key UNIQUE,
    row_version int4 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE wms.location_command (
    idempotency_key text
        CONSTRAINT location_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL,
    location_id uuid NOT NULL DEFAULT gen_random_uuid()
        CONSTRAINT location_command_location_id_key UNIQUE,
    CONSTRAINT location_command_canonical_command_check
        CHECK (octet_length(canonical_command) > 0)
);

CREATE TABLE wms.packaging (
    id uuid CONSTRAINT packaging_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    type text NOT NULL,
    code text NOT NULL CONSTRAINT packaging_code_key UNIQUE,
    location_id uuid NOT NULL CONSTRAINT packaging_location_id_fkey REFERENCES wms.location(id),
    lifecycle text NOT NULL DEFAULT 'open' CONSTRAINT packaging_lifecycle_check CHECK (lifecycle IN ('open', 'closed')),
    row_version int4 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT packaging_type_check CHECK (length(trim(type)) > 0),
    CONSTRAINT packaging_code_check CHECK (length(trim(code)) > 0)
);

CREATE TABLE wms.inventory (
    id uuid CONSTRAINT inventory_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id uuid NOT NULL CONSTRAINT inventory_product_id_fkey REFERENCES wms.product(id),
    packaging_id uuid NOT NULL CONSTRAINT inventory_packaging_id_fkey REFERENCES wms.packaging(id),
    location_id uuid NOT NULL CONSTRAINT inventory_location_id_fkey REFERENCES wms.location(id),
    quantity numeric NOT NULL,
    disposition text NOT NULL CONSTRAINT inventory_disposition_check CHECK (disposition IN ('available', 'held')),
    lifecycle text NOT NULL DEFAULT 'open' CONSTRAINT inventory_lifecycle_check CHECK (lifecycle IN ('open', 'closed')),
    row_version int4 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT inventory_quantity_lifecycle_check CHECK (
        (lifecycle = 'open' AND quantity > 0) OR (lifecycle = 'closed' AND quantity = 0)
    )
);

CREATE TABLE wms.inventory_transaction (
    id uuid CONSTRAINT inventory_transaction_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    operation_id uuid NOT NULL,
    type text NOT NULL CONSTRAINT inventory_transaction_type_check CHECK (type IN ('move', 'adjust', 'split', 'merge')),
    inventory_id uuid NOT NULL CONSTRAINT inventory_transaction_inventory_id_fkey REFERENCES wms.inventory(id),
    from_inventory_id uuid NOT NULL CONSTRAINT inventory_transaction_from_inventory_id_fkey REFERENCES wms.inventory(id),
    to_inventory_id uuid NOT NULL CONSTRAINT inventory_transaction_to_inventory_id_fkey REFERENCES wms.inventory(id),
    from_product_id uuid CONSTRAINT inventory_transaction_from_product_id_fkey REFERENCES wms.product(id),
    to_product_id uuid NOT NULL CONSTRAINT inventory_transaction_to_product_id_fkey REFERENCES wms.product(id),
    from_packaging_id uuid CONSTRAINT inventory_transaction_from_packaging_id_fkey REFERENCES wms.packaging(id),
    to_packaging_id uuid NOT NULL CONSTRAINT inventory_transaction_to_packaging_id_fkey REFERENCES wms.packaging(id),
    from_location_id uuid CONSTRAINT inventory_transaction_from_location_id_fkey REFERENCES wms.location(id),
    to_location_id uuid NOT NULL CONSTRAINT inventory_transaction_to_location_id_fkey REFERENCES wms.location(id),
    from_quantity numeric NOT NULL,
    to_quantity numeric NOT NULL,
    from_disposition text,
    to_disposition text NOT NULL,
    from_lifecycle text,
    to_lifecycle text NOT NULL,
    occurred_at timestamptz NOT NULL,
    reason text,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT inventory_transaction_operation_id_inventory_id_key UNIQUE (operation_id, inventory_id),
    CONSTRAINT inventory_transaction_type_reason_check CHECK (type <> 'adjust' OR (reason IS NOT NULL AND length(trim(reason)) > 0))
);

CREATE TABLE wms.inventory_move_command (
    idempotency_key text CONSTRAINT inventory_move_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL CONSTRAINT inventory_move_command_canonical_command_check CHECK (octet_length(canonical_command) > 0),
    operation_id uuid NOT NULL DEFAULT gen_random_uuid() CONSTRAINT inventory_move_command_operation_id_key UNIQUE,
    result text
);

CREATE TABLE wms.inventory_adjust_command (
    idempotency_key text CONSTRAINT inventory_adjust_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL CONSTRAINT inventory_adjust_command_canonical_command_check CHECK (octet_length(canonical_command) > 0),
    operation_id uuid NOT NULL DEFAULT gen_random_uuid() CONSTRAINT inventory_adjust_command_operation_id_key UNIQUE,
    result text
);

CREATE TABLE wms.inventory_split_command (
    idempotency_key text CONSTRAINT inventory_split_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL CONSTRAINT inventory_split_command_canonical_command_check CHECK (octet_length(canonical_command) > 0),
    operation_id uuid NOT NULL DEFAULT gen_random_uuid() CONSTRAINT inventory_split_command_operation_id_key UNIQUE,
    new_inventory_id uuid NOT NULL DEFAULT gen_random_uuid() CONSTRAINT inventory_split_command_new_inventory_id_key UNIQUE,
    result text
);

CREATE TABLE wms.inventory_merge_command (
    idempotency_key text CONSTRAINT inventory_merge_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL CONSTRAINT inventory_merge_command_canonical_command_check CHECK (octet_length(canonical_command) > 0),
    operation_id uuid NOT NULL DEFAULT gen_random_uuid() CONSTRAINT inventory_merge_command_operation_id_key UNIQUE,
    result text
);

CREATE TABLE wms.packaging_create_command (
    idempotency_key text CONSTRAINT packaging_create_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL CONSTRAINT packaging_create_command_canonical_command_check CHECK (octet_length(canonical_command) > 0),
    operation_id uuid NOT NULL DEFAULT gen_random_uuid() CONSTRAINT packaging_create_command_operation_id_key UNIQUE,
    packaging_id uuid NOT NULL DEFAULT gen_random_uuid() CONSTRAINT packaging_create_command_packaging_id_key UNIQUE,
    result text
);

CREATE TABLE wms.packaging_close_command (
    idempotency_key text CONSTRAINT packaging_close_command_idempotency_key_pkey PRIMARY KEY,
    canonical_command bytea NOT NULL CONSTRAINT packaging_close_command_canonical_command_check CHECK (octet_length(canonical_command) > 0),
    operation_id uuid NOT NULL DEFAULT gen_random_uuid() CONSTRAINT packaging_close_command_operation_id_key UNIQUE,
    result text
);
