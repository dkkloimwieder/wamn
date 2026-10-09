-- Remove the wiring activation pointer and its events
-- (docs/plan/platform-deploy.md §13, §15.1). upgrade-schema runs this file
-- once in one transaction and records it. A fresh install records it as
-- applied, because catalog-schema.sql no longer creates either table.
--
-- The runtime resolves wirings from the release it loaded and never read
-- activation, so nothing reads these rows.

DROP TABLE catalog.wiring_activation_events;
DROP TABLE catalog.wiring_activation;
