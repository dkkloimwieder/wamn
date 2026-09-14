CREATE TABLE history_probe.stock_item (
    id uuid CONSTRAINT stock_item_id_pkey PRIMARY KEY,
    sku text NOT NULL,
    quantity numeric NOT NULL,
    counted_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL
);
