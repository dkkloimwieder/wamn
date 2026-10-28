CREATE TABLE inventory.widget_maker (
    id uuid CONSTRAINT widget_maker_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    name text NOT NULL,
    edit_version int8 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE inventory.feed_order (
    id uuid CONSTRAINT feed_order_id_pkey PRIMARY KEY,
    label text NOT NULL
);

CREATE TABLE inventory.feed_item (
    id uuid CONSTRAINT feed_item_id_pkey PRIMARY KEY,
    label text NOT NULL
);

CREATE TABLE inventory.feed_line (
    id uuid CONSTRAINT feed_line_id_pkey PRIMARY KEY,
    order_id uuid CONSTRAINT feed_line_order_id_fkey REFERENCES inventory.feed_order(id) ON DELETE SET NULL,
    item_id uuid CONSTRAINT feed_line_item_id_fkey REFERENCES inventory.feed_item(id) ON DELETE SET NULL,
    label text NOT NULL,
    enabled boolean NOT NULL DEFAULT true
);
CREATE INDEX feed_line_order_id_idx ON inventory.feed_line(order_id);
CREATE INDEX feed_line_item_id_idx ON inventory.feed_line(item_id);

CREATE VIEW inventory.feed_screen AS
SELECT line.id, line.order_id, line.item_id, line.label, line.enabled,
       orders.label AS order_label, item.label AS item_label
FROM inventory.feed_line AS line
JOIN inventory.feed_order AS orders ON orders.id = line.order_id
JOIN inventory.feed_item AS item ON item.id = line.item_id;

CREATE TABLE inventory.widget (
    id uuid CONSTRAINT widget_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    code text NOT NULL CONSTRAINT widget_code_key UNIQUE,
    note text,
    maker_id uuid
        CONSTRAINT widget_maker_id_fkey
        REFERENCES inventory.widget_maker (id),
    edit_version int4 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT widget_code_check
        CHECK (code IN ('priority', 'standard'))
);

CREATE TABLE inventory.widget_tag (
    id uuid CONSTRAINT widget_tag_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    label text NOT NULL,
    edit_version int8 NOT NULL DEFAULT 1
);

CREATE TABLE inventory.numbered_label (
    id uuid CONSTRAINT numbered_label_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    number text NOT NULL CONSTRAINT numbered_label_number_key UNIQUE,
    stock_id text NOT NULL,
    edit_version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_by uuid NOT NULL
);
