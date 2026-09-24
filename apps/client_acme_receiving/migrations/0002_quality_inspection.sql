CREATE TABLE receiving.quality_inspection (
    receipt_id uuid
        CONSTRAINT quality_inspection_receipt_id_pkey PRIMARY KEY
        CONSTRAINT quality_inspection_receipt_id_fkey
        REFERENCES receiving.receipt (id),
    status text NOT NULL DEFAULT 'pending',
    row_version int4 NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    CONSTRAINT quality_inspection_status_check
        CHECK (status IN ('pending', 'approved'))
);
