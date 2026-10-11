-- Remove the component library of the control store (wamn-vavs4.7,
-- docs/plan/platform-deploy.md §7.4, §15.1, R3(1)). push-component, the one
-- writer of these tables, is gone, and no reader remains. The control store's
-- connection_requirements table stays until Epic 9, so only its foreign key to
-- the owner table drops. Dropping the tables also drops their policies and
-- triggers. No family holds a grant on them, so the control surface does not
-- change. A fresh install records this file as applied, because
-- control-portable-store.sql no longer creates the tables.

ALTER TABLE catalog.connection_requirements
    DROP CONSTRAINT connection_requirements_component_fkey;
DROP TABLE catalog.component_library;
DROP TABLE catalog.component_digest_owners;
