-- Remove the system copies of the package facts (wamn-unffv.1,
-- docs/plan/platform-deploy.md §17.2 Epic 9, R2). The project database holds
-- catalog.packages and catalog.package_migrations, and nothing reads or writes
-- the system copies. effective_release_packages had no writer, and it only
-- tied a release to the system packages and sealed their migrations, so it
-- goes too. catalog.effective_releases stays. Dropping the tables also drops
-- their policies and triggers, and the two seal functions drop after them. No
-- family holds a grant on them, so the control surface does not change. A
-- fresh install records this file as applied, because
-- control-portable-store.sql no longer creates the tables.

DROP TABLE catalog.effective_release_packages;
DROP TABLE catalog.package_migrations;
DROP TABLE catalog.packages;
DROP FUNCTION catalog.lock_package_coordinate_for_release_membership();
DROP FUNCTION catalog.reject_package_migration_after_release_membership();
