-- Stage evidence and stage progress are keyed by the package artifact digest,
-- the predecessor release digest and the evidence digest
-- (docs/plan/platform-deploy.md §10.2, wamn-snz0.3). The progress row drops
-- manifest_sha256 and qualification_sha256. A row written before this
-- migration has no package artifact digest to take, so the migration refuses
-- it rather than invent one (owner ruling of 2026-10-09: greenfield).
-- catalog.connection_requirements becomes a runtime projection that `apply`
-- materializes at install from the admitted descriptors in the package
-- artifacts (contract D, R3), so it drops its foreign key to the owner table.
-- upgrade-schema runs this file once in one transaction and records it. A
-- fresh install records it as applied, because catalog-schema.sql already
-- holds the new shape.
DO $refuse$
BEGIN
    IF EXISTS (SELECT 1 FROM catalog.package_upgrade_qualifications)
       OR EXISTS (SELECT 1 FROM catalog.package_upgrade_stages) THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'stage-digest-migration-unconvertible',
            DETAIL = 'stage evidence or stage progress predates the package artifact digest';
    END IF;
END
$refuse$;

ALTER TABLE catalog.connection_requirements
    DROP CONSTRAINT connection_requirements_component_fkey;

ALTER TABLE catalog.package_upgrade_qualifications
    ADD COLUMN package_artifact_digest text NOT NULL
        CHECK (package_artifact_digest ~ '^sha256:[0-9a-f]{64}$');

ALTER TABLE catalog.package_upgrade_stages
    DROP COLUMN manifest_sha256,
    DROP COLUMN qualification_sha256,
    ADD COLUMN package_artifact_digest text NOT NULL
        CHECK (package_artifact_digest ~ '^sha256:[0-9a-f]{64}$'),
    ADD COLUMN predecessor_release_digest text NOT NULL
        CHECK (predecessor_release_digest ~ '^sha256:[0-9a-f]{64}$'),
    ADD COLUMN evidence_digest text NOT NULL
        CHECK (evidence_digest ~ '^sha256:[0-9a-f]{64}$');

CREATE OR REPLACE FUNCTION catalog.guard_package_upgrade_stage_change()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-upgrade-stage-cannot-be-deleted';
    END IF;
    IF ROW(NEW.tenant_id, NEW.package_id, NEW.package_version, NEW.predecessor_version,
           NEW.package_artifact_digest, NEW.predecessor_release_digest, NEW.evidence_digest)
       IS DISTINCT FROM
       ROW(OLD.tenant_id, OLD.package_id, OLD.package_version, OLD.predecessor_version,
           OLD.package_artifact_digest, OLD.predecessor_release_digest, OLD.evidence_digest) THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-upgrade-stage-identity-is-immutable';
    END IF;
    IF OLD.status <> 'in_progress' THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-upgrade-stage-is-terminal';
    END IF;
    IF NEW.completed_batches < OLD.completed_batches
       OR NEW.completed_batches > OLD.completed_batches + 1 THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-upgrade-stage-invalid-batch-count';
    END IF;
    IF NEW.completed_batches = OLD.completed_batches AND NEW.cursor IS DISTINCT FROM OLD.cursor THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'package-upgrade-stage-cursor-requires-batch';
    END IF;
    RETURN NEW;
END
$$;
