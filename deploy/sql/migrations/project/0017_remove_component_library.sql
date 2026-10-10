-- Remove the component library of the project database (wamn-vavs4.7,
-- docs/plan/platform-deploy.md §7.4, §15.1, R3(1)). The release manifest
-- carries the component descriptors, and the runtime reads them there, so no
-- reader of these tables remains and push-component, their one writer, is
-- gone. Dropping the tables also drops their policies, triggers, indexes and
-- grants. connection_requirements lost its foreign key to the owner table in
-- 0014. A fresh install records this file as applied, because
-- catalog-schema.sql no longer creates the tables.

DROP TABLE catalog.component_library;
DROP TABLE catalog.component_digest_owners;
