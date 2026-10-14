-- One permission row per grant (docs/plan/trx-pkg-plan.md, Authorization):
-- a caller holds exactly the rows of its roles, so the vestigial
-- `required_by` column goes with its self-reference and its pattern check.
-- Every stored row already has `permission = required_by` (0011), so no row
-- is lost and the new key stays unique. upgrade-schema runs this file once in
-- one transaction and records it. A fresh install records it as applied,
-- because app-schema.sql already holds the new shape.

ALTER TABLE app_system.permissions
    DROP CONSTRAINT permissions_required_by_fkey,
    DROP CONSTRAINT permissions_reference_check,
    DROP CONSTRAINT permissions_pkey,
    DROP COLUMN required_by,
    ADD PRIMARY KEY (tenant_id, role_name, permission),
    ADD CONSTRAINT permissions_reference_check
        CHECK (permission ~ '^[a-z][a-z0-9]*(-[a-z0-9]+)*:[a-z][a-z0-9]*(-[a-z0-9]+)*/[a-z][a-z0-9]*(-[a-z0-9]+)*$');
