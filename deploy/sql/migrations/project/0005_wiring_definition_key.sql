-- The same wiring graph is legal under two package versions of one package,
-- as the primary key of catalog.wirings already says, so the definition key
-- names the package version too (owner ruling of 2026-10-02, wamn-ld93.21).
-- upgrade-schema runs this file once in one transaction and records it. A
-- fresh install records it as applied, because catalog-schema.sql already
-- carries this key.
ALTER TABLE catalog.wirings DROP CONSTRAINT wirings_definition_key;
ALTER TABLE catalog.wirings ADD CONSTRAINT wirings_definition_key
    UNIQUE (tenant_id, package_id, package_version, wiring_id, wiring_hash);
