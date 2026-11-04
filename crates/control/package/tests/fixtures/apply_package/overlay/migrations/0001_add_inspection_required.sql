ALTER TABLE inventory.panel
    ADD COLUMN overlay_inspection_required boolean
    NOT NULL DEFAULT false;

ALTER TABLE inventory.panel
    ADD COLUMN overlay_quality_status text
    NOT NULL DEFAULT 'not_required';

ALTER TABLE inventory.panel
    ADD CONSTRAINT panel_overlay_quality_status_check
    CHECK (overlay_quality_status IN (
        'not_required', 'pending', 'approved'
    ));
