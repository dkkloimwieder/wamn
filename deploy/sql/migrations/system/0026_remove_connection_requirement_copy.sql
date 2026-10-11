-- Remove the system copy of catalog.connection_requirements (wamn-jqdja.1,
-- docs/plan/platform-deploy.md §17.2 Epic 9, R2). The project database holds
-- the rows, which env apply writes from the admitted descriptors of the
-- release manifest, and analysis reads the descriptors from the manifest.
-- Dropping the table also drops its policies, its trigger and the control
-- author's read of it. A fresh install records this file as applied, because
-- control-portable-store.sql no longer creates the table.

DROP TABLE catalog.connection_requirements;
