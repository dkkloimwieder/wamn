ALTER TABLE receiving.purchase_order
    ADD COLUMN acme_inspection_required boolean
    NOT NULL DEFAULT false;

ALTER TABLE receiving.purchase_order
    ADD COLUMN acme_quality_status text
    NOT NULL DEFAULT 'not_required';

ALTER TABLE receiving.purchase_order
    ADD CONSTRAINT purchase_order_acme_quality_status_check
    CHECK (acme_quality_status IN (
        'not_required', 'pending', 'approved'
    ));
