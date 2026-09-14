CREATE TABLE whole_row_probe.item (
    id uuid CONSTRAINT item_id_pkey PRIMARY KEY,
    sku text NOT NULL,
    note text NOT NULL
);

CREATE TABLE whole_row_probe.line (
    id uuid CONSTRAINT line_id_pkey PRIMARY KEY,
    item uuid NOT NULL,
    quantity numeric NOT NULL
);
