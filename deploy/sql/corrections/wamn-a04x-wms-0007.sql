-- Incident wamn-a04x: owner review is required before execution.
-- Apply only to the WMS database after the owner approves the correction.
-- Preserve migration 6 and the shared catalog.reject_immutable_row_change function.

BEGIN;

DO $assert_migration$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM app_system.schema_migrations
         WHERE ordinal = 7
           AND relative_path = 'migrations/project/0007_package_upgrade_qualifications.sql'
           AND sha256 = 'sha256:f47497e245b6475456ba3ce96573ba9556930f5c2335818bc3b09e675f52efe2'
    ) THEN
        RAISE EXCEPTION 'wamn-a04x correction refused: exact migration 0007_package_upgrade_qualifications record is absent';
    END IF;
    IF current_database() <> 'wamn-db-dkk--wms--dev--0nk1lrpr' THEN
        RAISE EXCEPTION 'wamn-a04x correction refused: unexpected database %', current_database();
    END IF;
END;
$assert_migration$;

LOCK TABLE app_system.schema_migrations IN EXCLUSIVE MODE;

-- Remove the objects in reverse creation order. The shared function stays.
DROP TRIGGER package_upgrade_qualifications_immutable
    ON catalog.package_upgrade_qualifications;
DROP INDEX catalog.package_upgrade_qualifications_tkey RESTRICT;
DROP POLICY package_upgrade_qualifications_platform
    ON catalog.package_upgrade_qualifications;
DROP POLICY package_upgrade_qualifications_tenant
    ON catalog.package_upgrade_qualifications;
-- Dropping the table removes its constraints and row security configuration.
DROP TABLE catalog.package_upgrade_qualifications RESTRICT;

DO $delete_migration$
DECLARE
    removed_rows bigint;
BEGIN
    DELETE FROM app_system.schema_migrations
     WHERE ordinal = 7
       AND relative_path = 'migrations/project/0007_package_upgrade_qualifications.sql'
       AND sha256 = 'sha256:f47497e245b6475456ba3ce96573ba9556930f5c2335818bc3b09e675f52efe2';
    GET DIAGNOSTICS removed_rows = ROW_COUNT;
    IF removed_rows <> 1 THEN
        RAISE EXCEPTION 'wamn-a04x correction refused: removed % tracking rows instead of 1', removed_rows;
    END IF;
END;
$delete_migration$;

COMMIT;
