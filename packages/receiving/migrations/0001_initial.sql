CREATE TABLE receiving.item (
    id uuid CONSTRAINT item_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    item_number text NOT NULL CONSTRAINT item_item_number_key UNIQUE
);

CREATE TABLE receiving.location (
    id uuid CONSTRAINT location_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    location_code text NOT NULL CONSTRAINT location_location_code_key UNIQUE
);

CREATE TABLE receiving.purchase_order (
    id uuid CONSTRAINT purchase_order_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    purchase_order_number text NOT NULL
        CONSTRAINT purchase_order_purchase_order_number_key UNIQUE,
    supplier_id uuid NOT NULL,
    status text NOT NULL DEFAULT 'open',
    row_version int8 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT purchase_order_status_check
        CHECK (status IN ('open', 'complete', 'cancelled'))
);

CREATE TABLE receiving.purchase_order_line (
    id uuid CONSTRAINT purchase_order_line_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    purchase_order_id uuid NOT NULL
        CONSTRAINT purchase_order_line_purchase_order_id_fkey
        REFERENCES receiving.purchase_order (id),
    line_number int4 NOT NULL,
    item_id uuid NOT NULL
        CONSTRAINT purchase_order_line_item_id_fkey
        REFERENCES receiving.item (id),
    ordered_quantity numeric NOT NULL,
    received_quantity numeric NOT NULL DEFAULT 0,
    CONSTRAINT purchase_order_line_purchase_order_id_line_number_key
        UNIQUE (purchase_order_id, line_number),
    CONSTRAINT purchase_order_line_ordered_quantity_check
        CHECK (ordered_quantity > 0),
    CONSTRAINT purchase_order_line_ordered_quantity_received_quantity_check
        CHECK (
            received_quantity >= 0
            AND received_quantity <= ordered_quantity
        )
);

CREATE TABLE receiving.receipt (
    id uuid CONSTRAINT receipt_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    idempotency_key text NOT NULL
        CONSTRAINT receipt_idempotency_key_key UNIQUE,
    purchase_order_id uuid NOT NULL
        CONSTRAINT receipt_purchase_order_id_fkey
        REFERENCES receiving.purchase_order (id),
    receipt_reference text NOT NULL,
    occurred_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT receipt_purchase_order_id_receipt_reference_key
        UNIQUE (purchase_order_id, receipt_reference)
);

CREATE TABLE receiving.receipt_line (
    id uuid CONSTRAINT receipt_line_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    receipt_id uuid NOT NULL
        CONSTRAINT receipt_line_receipt_id_fkey
        REFERENCES receiving.receipt (id),
    purchase_order_line_id uuid NOT NULL
        CONSTRAINT receipt_line_purchase_order_line_id_fkey
        REFERENCES receiving.purchase_order_line (id),
    quantity numeric NOT NULL,
    location_id uuid NOT NULL
        CONSTRAINT receipt_line_location_id_fkey
        REFERENCES receiving.location (id),
    CONSTRAINT receipt_line_quantity_check CHECK (quantity > 0)
);
