-- copy-project-env is removed (wamn-snz0.6, docs/plan/platform-deploy.md
-- section 12.1): a copy is the same environment document under another
-- coordinate, applied by env apply. The verb installed its operations state on
-- first use only, so a database where it never ran holds neither table, and
-- the stable wamn_ops role loses its USAGE on provisioning where it exists.
DROP TABLE IF EXISTS provisioning.copy_sagas;
DROP TABLE IF EXISTS provisioning.dumps;
DO $ops$
BEGIN
    IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_ops') THEN
        REVOKE ALL PRIVILEGES ON SCHEMA provisioning FROM wamn_ops;
    END IF;
END
$ops$;
