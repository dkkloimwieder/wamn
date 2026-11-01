CREATE TABLE receiving.item (
    id uuid CONSTRAINT item_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    item_number text NOT NULL CONSTRAINT item_item_number_key UNIQUE
);

CREATE TABLE receiving.location (
    id uuid CONSTRAINT location_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    location_code text NOT NULL CONSTRAINT location_location_code_key UNIQUE
);

CREATE TABLE receiving.supplier (
    id uuid CONSTRAINT supplier_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    name text NOT NULL CONSTRAINT supplier_name_check CHECK (char_length(btrim(name)) >= 1),
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE receiving.purchase_order (
    id uuid CONSTRAINT purchase_order_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    purchase_order_number text NOT NULL
        CONSTRAINT purchase_order_purchase_order_number_key UNIQUE,
    supplier_id uuid NOT NULL
        CONSTRAINT purchase_order_supplier_id_fkey
        REFERENCES receiving.supplier (id),
    status text NOT NULL DEFAULT 'open',
    row_version int4 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
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
    purchase_order_id uuid NOT NULL
        CONSTRAINT receipt_purchase_order_id_fkey
        REFERENCES receiving.purchase_order (id),
    receipt_reference text NOT NULL,
    occurred_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
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

CREATE INDEX purchase_order_id_created_at_idx ON receiving.purchase_order (created_at, id);
CREATE INDEX purchase_order_id_purchase_order_number_idx ON receiving.purchase_order (purchase_order_number, id);
CREATE INDEX purchase_order_id_status_idx ON receiving.purchase_order (status, id);
CREATE INDEX receipt_id_created_at_idx ON receiving.receipt (created_at, id);
CREATE INDEX supplier_id_created_at_idx ON receiving.supplier (created_at, id);

CREATE VIEW receiving.receipt_screen_line AS
SELECT
    line.id AS line_id,
    line.purchase_order_id,
    purchase_order.purchase_order_number,
    purchase_order.status AS purchase_order_status,
    purchase_order.supplier_id,
    purchase_order.row_version AS purchase_order_row_version,
    line.line_number,
    line.item_id,
    item.item_number,
    line.ordered_quantity,
    line.received_quantity,
    line.ordered_quantity - line.received_quantity AS remaining_quantity
FROM receiving.purchase_order_line AS line
JOIN receiving.purchase_order AS purchase_order
    ON purchase_order.id = line.purchase_order_id
JOIN receiving.item AS item
    ON item.id = line.item_id;

CREATE INDEX purchase_order_line_item_id_idx ON receiving.purchase_order_line (item_id);

CREATE VIEW receiving.purchase_order_receiving_summary AS
SELECT
    purchase_order.id AS purchase_order_id,
    purchase_order.purchase_order_number,
    purchase_order.status,
    purchase_order.supplier_id,
    supplier.name AS supplier_name,
    count(line.id) AS line_count,
    coalesce(sum(line.ordered_quantity), 0) AS ordered_quantity,
    coalesce(sum(line.received_quantity), 0) AS received_quantity
FROM receiving.purchase_order AS purchase_order
JOIN receiving.supplier AS supplier
    ON supplier.id = purchase_order.supplier_id
LEFT JOIN receiving.purchase_order_line AS line
    ON line.purchase_order_id = purchase_order.id
GROUP BY purchase_order.id, supplier.name;
