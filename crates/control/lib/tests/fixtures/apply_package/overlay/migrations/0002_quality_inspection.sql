CREATE TABLE inventory.quality_inspection (
    rack_id uuid
        CONSTRAINT quality_inspection_rack_id_pkey PRIMARY KEY
        CONSTRAINT quality_inspection_rack_id_fkey
        REFERENCES inventory.rack (id),
    status text NOT NULL DEFAULT 'pending',
    row_version int8 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    CONSTRAINT quality_inspection_status_check
        CHECK (status IN ('pending', 'approved'))
);
