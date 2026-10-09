-- A release is named by its manifest digest (docs/plan/platform-deploy.md R1,
-- wamn-snz0.5). catalog.releases is the immutable digest-keyed cache of the
-- canonical manifest bytes. It replaces catalog.effective_releases,
-- effective_release_packages, release_components and
-- release_manifest_snapshots. Every row that pinned an integer release id pins
-- the digest instead: the release head, the connection bindings, the package
-- upgrade evidence and the admission pin of wamn_run.runs.
--
-- The conversion copies each frozen snapshot into catalog.releases and gives
-- each pinning row the digest of its release's snapshot. A head or a binding
-- whose release has no snapshot has no digest to take, and the migration
-- refuses it rather than dropping it. A run whose release has no snapshot, and
-- a candidate run, keeps no pin. upgrade-schema runs this file once in one
-- transaction and records it. A fresh install records it as applied, because
-- the full schema files already hold the new shape.

CREATE TABLE catalog.releases (
    tenant_id       text        NOT NULL CHECK (tenant_id <> ''),
    manifest_digest text        NOT NULL CHECK (manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    canonical_bytes bytea       NOT NULL CHECK (octet_length(canonical_bytes) > 0),
    recorded_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT releases_pkey PRIMARY KEY (tenant_id, manifest_digest),
    CONSTRAINT releases_exact_hash
        CHECK (manifest_digest = 'sha256:' || encode(sha256(canonical_bytes), 'hex'))
);
DO $owner$
BEGIN
    EXECUTE format('ALTER TABLE catalog.releases OWNER TO %s',
                   (SELECT relowner::regrole::text FROM pg_catalog.pg_class
                     WHERE oid = 'catalog.packages'::regclass));
END
$owner$;
INSERT INTO catalog.releases (tenant_id, manifest_digest, canonical_bytes)
SELECT tenant_id, manifest_digest, canonical_bytes
  FROM catalog.release_manifest_snapshots
    ON CONFLICT DO NOTHING;
ALTER TABLE catalog.releases ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.releases FORCE ROW LEVEL SECURITY;
CREATE POLICY releases_tenant ON catalog.releases TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY releases_platform ON catalog.releases
    AS PERMISSIVE FOR ALL TO wamn_platform USING (true) WITH CHECK (true);
CREATE INDEX releases_tkey ON catalog.releases ((wamn_authority.tenant_key(tenant_id)));
CREATE TRIGGER releases_immutable
    BEFORE UPDATE OR DELETE ON catalog.releases
    FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
REVOKE ALL ON catalog.releases FROM PUBLIC;
GRANT SELECT ON catalog.releases TO wamn_app;

-- The release head names its release by digest.
ALTER TABLE catalog.effective_release_heads
    DROP CONSTRAINT effective_release_heads_release_fkey,
    ADD COLUMN manifest_digest text;
UPDATE catalog.effective_release_heads AS head
   SET manifest_digest = snapshot.manifest_digest
  FROM catalog.release_manifest_snapshots AS snapshot
 WHERE snapshot.tenant_id = head.tenant_id
   AND snapshot.effective_release_id = head.effective_release_id;
DO $heads$
BEGIN
    IF EXISTS (SELECT 1 FROM catalog.effective_release_heads WHERE manifest_digest IS NULL) THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'release-digest-migration-unconvertible',
            DETAIL = 'a release head names a release with no frozen manifest snapshot';
    END IF;
END
$heads$;
ALTER TABLE catalog.effective_release_heads
    DROP COLUMN effective_release_id,
    ALTER COLUMN manifest_digest SET NOT NULL,
    ADD CONSTRAINT effective_release_heads_manifest_digest_check
        CHECK (manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    ADD CONSTRAINT effective_release_heads_release_fkey
        FOREIGN KEY (tenant_id, manifest_digest)
        REFERENCES catalog.releases (tenant_id, manifest_digest);

-- A connection binding is keyed by the release digest.
ALTER TABLE catalog.connection_bindings
    DROP CONSTRAINT connection_bindings_release_fkey,
    DROP CONSTRAINT connection_bindings_pkey,
    ADD COLUMN manifest_digest text;
ALTER TABLE catalog.connection_bindings DISABLE TRIGGER connection_bindings_immutable;
UPDATE catalog.connection_bindings AS binding
   SET manifest_digest = snapshot.manifest_digest
  FROM catalog.release_manifest_snapshots AS snapshot
 WHERE snapshot.tenant_id = binding.tenant_id
   AND snapshot.effective_release_id = binding.effective_release_id;
ALTER TABLE catalog.connection_bindings ENABLE TRIGGER connection_bindings_immutable;
DO $bindings$
BEGIN
    IF EXISTS (SELECT 1 FROM catalog.connection_bindings WHERE manifest_digest IS NULL) THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'release-digest-migration-unconvertible',
            DETAIL = 'a connection binding names a release with no frozen manifest snapshot';
    END IF;
END
$bindings$;
ALTER TABLE catalog.connection_bindings
    DROP COLUMN effective_release_id,
    ALTER COLUMN manifest_digest SET NOT NULL,
    ADD CONSTRAINT connection_bindings_manifest_digest_check
        CHECK (manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    ADD CONSTRAINT connection_bindings_pkey
        PRIMARY KEY (tenant_id, manifest_digest, component_digest, store_alias),
    ADD CONSTRAINT connection_bindings_release_fkey
        FOREIGN KEY (tenant_id, manifest_digest)
        REFERENCES catalog.releases (tenant_id, manifest_digest);

-- Upgrade evidence names its predecessor release by digest only.
ALTER TABLE catalog.package_upgrade_qualifications
    DROP CONSTRAINT package_upgrade_qualifications_predecessor_fkey,
    DROP COLUMN predecessor_release_id,
    ADD CONSTRAINT package_upgrade_qualifications_predecessor_fkey
        FOREIGN KEY (tenant_id, predecessor_manifest_digest)
        REFERENCES catalog.releases (tenant_id, manifest_digest);

-- The admission pin of a run is the release digest, written at admission and
-- immutable after it.
ALTER TABLE wamn_run.runs DROP CONSTRAINT runs_release_fk;
ALTER TABLE wamn_run.runs DISABLE TRIGGER runs_admission_pins_immutable;
UPDATE wamn_run.runs AS run
   SET manifest_digest = CASE
           WHEN run.binding_world_json IS NULL THEN (
               SELECT snapshot.manifest_digest
                 FROM catalog.release_manifest_snapshots AS snapshot
                WHERE snapshot.tenant_id = run.tenant_id
                  AND snapshot.effective_release_id = run.effective_release_id)
           ELSE NULL
       END;
ALTER TABLE wamn_run.runs ENABLE TRIGGER runs_admission_pins_immutable;
DROP TRIGGER runs_admission_pins_immutable ON wamn_run.runs;
DROP INDEX wamn_run.runs_release;
ALTER TABLE wamn_run.runs
    DROP CONSTRAINT runs_check,
    DROP COLUMN effective_release_id,
    ADD CONSTRAINT runs_check CHECK (package_id <> '' AND environment <> ''),
    ADD CONSTRAINT runs_release_fk
        FOREIGN KEY (tenant_id, manifest_digest)
        REFERENCES catalog.releases (tenant_id, manifest_digest);
CREATE OR REPLACE FUNCTION wamn_run.guard_run_admission_pins_immutable()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.flow_id IS DISTINCT FROM OLD.flow_id
       OR NEW.flow_version IS DISTINCT FROM OLD.flow_version
       OR NEW.package_id IS DISTINCT FROM OLD.package_id
       OR NEW.environment IS DISTINCT FROM OLD.environment
       OR NEW.capture_mode IS DISTINCT FROM OLD.capture_mode
       OR NEW.durability_class IS DISTINCT FROM OLD.durability_class
       OR NEW.wiring_id IS DISTINCT FROM OLD.wiring_id
       OR NEW.wiring_version IS DISTINCT FROM OLD.wiring_version
       OR NEW.wiring_hash IS DISTINCT FROM OLD.wiring_hash
       OR NEW.binding_world_json IS DISTINCT FROM OLD.binding_world_json
       OR NEW.manifest_digest IS DISTINCT FROM OLD.manifest_digest
       OR NEW.service_principal_id IS DISTINCT FROM OLD.service_principal_id THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'run-admission-pin-immutable';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER runs_admission_pins_immutable
BEFORE UPDATE OF flow_id, flow_version, package_id, environment,
                 capture_mode, durability_class, wiring_id, wiring_version,
                 wiring_hash, binding_world_json, manifest_digest, service_principal_id
ON wamn_run.runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_run_admission_pins_immutable();

-- The executor family inserts the pin and no longer writes it at claim.
DO $executor$
BEGIN
    IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_executor_platform') THEN
        REVOKE UPDATE (manifest_digest) ON TABLE wamn_run.runs FROM wamn_executor_platform;
        GRANT INSERT (manifest_digest) ON TABLE wamn_run.runs TO wamn_executor_platform;
        GRANT SELECT ON TABLE catalog.releases TO wamn_executor_platform;
    END IF;
    IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_http_admitter') THEN
        GRANT SELECT ON TABLE catalog.releases TO wamn_http_admitter;
    END IF;
END
$executor$;

-- The release closure is read from the manifest bytes.
DROP TRIGGER effective_release_packages_seal_coordinate ON catalog.effective_release_packages;
DROP TABLE catalog.release_components;
DROP FUNCTION catalog.guard_release_component_insert();
DROP TABLE catalog.release_manifest_snapshots;
DROP TABLE catalog.effective_release_packages;
DROP TABLE catalog.effective_releases;

CREATE OR REPLACE FUNCTION catalog.lock_package_coordinate_for_release_membership()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM 1
      FROM catalog.packages AS package
      JOIN jsonb_array_elements(
               convert_from(NEW.canonical_bytes, 'UTF8')::jsonb #> '{release,packages}'
           ) AS member(value)
        ON package.package_id = member.value ->> 'package-id'
       AND package.package_version = member.value ->> 'package-version'
     WHERE package.tenant_id = NEW.tenant_id
     FOR UPDATE OF package;
    RETURN NEW;
END
$$;
CREATE TRIGGER releases_seal_coordinate
    BEFORE INSERT ON catalog.releases
    FOR EACH ROW
    EXECUTE FUNCTION catalog.lock_package_coordinate_for_release_membership();

CREATE OR REPLACE FUNCTION catalog.reject_package_migration_after_release_membership()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM 1
      FROM catalog.packages
     WHERE tenant_id = NEW.tenant_id
       AND package_id = NEW.package_id
       AND package_version = NEW.package_version
     FOR UPDATE;

    IF EXISTS (
        SELECT 1
          FROM catalog.releases AS release
          CROSS JOIN LATERAL jsonb_array_elements(
               convert_from(release.canonical_bytes, 'UTF8')::jsonb #> '{release,packages}'
          ) AS member(value)
         WHERE release.tenant_id = NEW.tenant_id
           AND member.value ->> 'package-id' = NEW.package_id
           AND member.value ->> 'package-version' = NEW.package_version
    ) AND NOT EXISTS (
        SELECT 1
          FROM pg_catalog.pg_database
         WHERE datname = pg_catalog.current_database()
           AND pg_catalog.shobj_description(oid, 'pg_database')
               = pg_catalog.current_setting('wamn.local_target_comment', true)
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'package-version-sealed',
            DETAIL = format(
                'coordinate=%s@%s belongs to an effective release',
                NEW.package_id, NEW.package_version
            ),
            HINT = 'create and apply a new package version for additional migrations';
    END IF;
    RETURN NEW;
END
$$;
