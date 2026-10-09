-- A release is named by its manifest digest (docs/plan/platform-deploy.md R1,
-- wamn-snz0.5). The control store keys its release identity, membership, head,
-- deployment attestations and selections by the digest, and no integer release
-- id remains.
--
-- The control store holds no manifest bytes. The digest of a release is the
-- manifest hash that its deployment attestation records, so a release id
-- converts when its attestations name exactly one digest. A release id with no
-- attestation, or with attestations of more than one digest, has no digest to
-- take, and the migration refuses it rather than dropping its rows. The
-- tables force row level security, which hides every row from the owner, so
-- the conversion lifts the force for its own transaction and restores it.

ALTER TABLE catalog.effective_releases NO FORCE ROW LEVEL SECURITY;
ALTER TABLE catalog.effective_release_packages NO FORCE ROW LEVEL SECURITY;
ALTER TABLE catalog.effective_release_heads NO FORCE ROW LEVEL SECURITY;
ALTER TABLE catalog.deployment_attestations NO FORCE ROW LEVEL SECURITY;
ALTER TABLE catalog.release_selections NO FORCE ROW LEVEL SECURITY;

DO $unconvertible$
BEGIN
    IF EXISTS (
        SELECT 1
          FROM catalog.effective_releases AS release
         WHERE (SELECT count(DISTINCT attestation.deployed_manifest_hash)
                  FROM catalog.deployment_attestations AS attestation
                 WHERE attestation.tenant_id = release.tenant_id
                   AND attestation.effective_release_id = release.effective_release_id) <> 1
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'release-digest-migration-unconvertible',
            DETAIL = 'a release id has no deployment attestation, or attestations of more than one manifest digest';
    END IF;
END
$unconvertible$;

ALTER TABLE catalog.effective_release_packages
    DROP CONSTRAINT effective_release_packages_release_fkey;
ALTER TABLE catalog.effective_release_heads
    DROP CONSTRAINT effective_release_heads_release_fkey;
ALTER TABLE catalog.deployment_attestations
    DROP CONSTRAINT deployment_attestations_release_fkey;
ALTER TABLE catalog.release_selections
    DROP CONSTRAINT release_selections_release_fkey;

ALTER TABLE catalog.effective_releases ADD COLUMN manifest_digest text;
ALTER TABLE catalog.effective_release_packages ADD COLUMN manifest_digest text;
ALTER TABLE catalog.effective_release_heads ADD COLUMN manifest_digest text;
ALTER TABLE catalog.deployment_attestations ADD COLUMN manifest_digest text;
ALTER TABLE catalog.release_selections ADD COLUMN manifest_digest text;

ALTER TABLE catalog.effective_releases DISABLE TRIGGER effective_releases_immutable;
ALTER TABLE catalog.effective_release_packages DISABLE TRIGGER effective_release_packages_immutable;
ALTER TABLE catalog.deployment_attestations DISABLE TRIGGER deployment_attestations_immutable;
ALTER TABLE catalog.release_selections DISABLE TRIGGER release_selections_immutable;

UPDATE catalog.effective_releases AS release
   SET manifest_digest = (
       SELECT DISTINCT attestation.deployed_manifest_hash
         FROM catalog.deployment_attestations AS attestation
        WHERE attestation.tenant_id = release.tenant_id
          AND attestation.effective_release_id = release.effective_release_id);
UPDATE catalog.effective_release_packages AS member
   SET manifest_digest = release.manifest_digest
  FROM catalog.effective_releases AS release
 WHERE release.tenant_id = member.tenant_id
   AND release.effective_release_id = member.effective_release_id;
UPDATE catalog.effective_release_heads AS head
   SET manifest_digest = release.manifest_digest
  FROM catalog.effective_releases AS release
 WHERE release.tenant_id = head.tenant_id
   AND release.effective_release_id = head.effective_release_id;
UPDATE catalog.deployment_attestations AS attestation
   SET manifest_digest = release.manifest_digest
  FROM catalog.effective_releases AS release
 WHERE release.tenant_id = attestation.tenant_id
   AND release.effective_release_id = attestation.effective_release_id;
UPDATE catalog.release_selections AS selection
   SET manifest_digest = release.manifest_digest
  FROM catalog.effective_releases AS release
 WHERE release.tenant_id = selection.tenant_id
   AND release.effective_release_id = selection.effective_release_id;

ALTER TABLE catalog.effective_releases ENABLE TRIGGER effective_releases_immutable;
ALTER TABLE catalog.effective_release_packages ENABLE TRIGGER effective_release_packages_immutable;
ALTER TABLE catalog.deployment_attestations ENABLE TRIGGER deployment_attestations_immutable;
ALTER TABLE catalog.release_selections ENABLE TRIGGER release_selections_immutable;

ALTER TABLE catalog.effective_releases
    DROP CONSTRAINT effective_releases_pkey,
    DROP CONSTRAINT effective_releases_environment_key,
    DROP COLUMN effective_release_id,
    ALTER COLUMN manifest_digest SET NOT NULL,
    ADD CONSTRAINT effective_releases_manifest_digest_check
        CHECK (manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    ADD CONSTRAINT effective_releases_pkey PRIMARY KEY (tenant_id, manifest_digest),
    ADD CONSTRAINT effective_releases_environment_key
        UNIQUE (tenant_id, manifest_digest, environment);

ALTER TABLE catalog.effective_release_packages
    DROP CONSTRAINT effective_release_packages_pkey,
    DROP CONSTRAINT effective_release_packages_exact_pair_key,
    DROP COLUMN effective_release_id,
    ALTER COLUMN manifest_digest SET NOT NULL,
    ADD CONSTRAINT effective_release_packages_manifest_digest_check
        CHECK (manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    ADD CONSTRAINT effective_release_packages_pkey
        PRIMARY KEY (tenant_id, manifest_digest, package_id),
    ADD CONSTRAINT effective_release_packages_exact_pair_key
        UNIQUE (tenant_id, manifest_digest, package_id, package_version),
    ADD CONSTRAINT effective_release_packages_release_fkey
        FOREIGN KEY (tenant_id, manifest_digest)
        REFERENCES catalog.effective_releases (tenant_id, manifest_digest);

ALTER TABLE catalog.effective_release_heads
    DROP COLUMN effective_release_id,
    ALTER COLUMN manifest_digest SET NOT NULL,
    ADD CONSTRAINT effective_release_heads_manifest_digest_check
        CHECK (manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    ADD CONSTRAINT effective_release_heads_release_fkey
        FOREIGN KEY (tenant_id, manifest_digest, environment)
        REFERENCES catalog.effective_releases (tenant_id, manifest_digest, environment);

ALTER TABLE catalog.deployment_attestations
    DROP CONSTRAINT deployment_attestations_coordinate,
    DROP COLUMN effective_release_id,
    ALTER COLUMN manifest_digest SET NOT NULL,
    ADD CONSTRAINT deployment_attestations_manifest_digest_check
        CHECK (manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    ADD CONSTRAINT deployment_attestations_coordinate UNIQUE (
        tenant_id, environment_instance, manifest_digest, org_id, project_id, environment
    ),
    ADD CONSTRAINT deployment_attestations_release_fkey
        FOREIGN KEY (tenant_id, manifest_digest, environment)
        REFERENCES catalog.effective_releases (tenant_id, manifest_digest, environment);

ALTER TABLE catalog.release_selections
    DROP COLUMN effective_release_id,
    ALTER COLUMN manifest_digest SET NOT NULL,
    ADD CONSTRAINT release_selections_manifest_digest_check
        CHECK (manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    ADD CONSTRAINT release_selections_release_fkey
        FOREIGN KEY (tenant_id, manifest_digest, environment)
        REFERENCES catalog.effective_releases (tenant_id, manifest_digest, environment);

ALTER TABLE catalog.effective_releases FORCE ROW LEVEL SECURITY;
ALTER TABLE catalog.effective_release_packages FORCE ROW LEVEL SECURITY;
ALTER TABLE catalog.effective_release_heads FORCE ROW LEVEL SECURITY;
ALTER TABLE catalog.deployment_attestations FORCE ROW LEVEL SECURITY;
ALTER TABLE catalog.release_selections FORCE ROW LEVEL SECURITY;
