CREATE TABLE receiving.quality_inspection (
    receipt_id uuid
        CONSTRAINT quality_inspection_receipt_id_pkey PRIMARY KEY
        CONSTRAINT quality_inspection_receipt_id_fkey
        REFERENCES receiving.receipt (id),
    status text NOT NULL DEFAULT 'pending',
    row_version int8 NOT NULL DEFAULT 1,
    CONSTRAINT quality_inspection_status_check
        CHECK (status IN ('pending', 'approved'))
);
