-- One package identity (wamn-vavs4.4, docs/plan/platform-deploy.md R3(1),
-- §15.1): catalog.packages records the package artifact digest, the digest
-- of the verified package artifact that apply installed, in place of the
-- wamn.json hash. A row that wamn dev writes from a working tree has no
-- artifact, so its digest is NULL, and every release verb refuses that row.
-- A row written before this migration has no artifact digest to take, so the
-- migration refuses it rather than invent one (owner ruling of 2026-10-10:
-- refuse when rows exist, recreate the environment, no empty digests). upgrade-schema runs this file once in one
-- transaction and records it. A fresh install records it as applied, because
-- catalog-schema.sql already holds the new shape.
DO $refuse$
BEGIN
    IF EXISTS (SELECT 1 FROM catalog.packages) THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'package-artifact-digest-migration-unconvertible',
            DETAIL = 'catalog.packages holds rows that predate the package artifact digest';
    END IF;
END
$refuse$;

ALTER TABLE catalog.packages
    DROP COLUMN manifest_sha256,
    ADD COLUMN artifact_digest text
        CONSTRAINT packages_artifact_digest_check
        CHECK (artifact_digest IS NULL OR artifact_digest ~ '^sha256:[0-9a-f]{64}$');
