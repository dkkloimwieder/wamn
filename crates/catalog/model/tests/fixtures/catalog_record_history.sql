BEGIN;
SELECT set_config('app.user_id', '00000000-0000-4000-8000-0000000000f1', true),
       set_config('app.operation', 'admin:seed-catalog-history', true);
INSERT INTO platform."user" (id, type, email, display_name) VALUES
    ('00000000-0000-4000-8000-0000000000f1', 'user', 'creator@example.invalid', 'creator'),
    ('00000000-0000-4000-8000-0000000000f2', 'user', 'editor@example.invalid', 'editor');
INSERT INTO catalog.packages (env_key, package_id, package_version, predecessor_version)
VALUES ('history', 'fixture', '1', NULL), ('history', 'fixture', '2', '1');
INSERT INTO catalog.package_migrations
    (env_key, package_id, package_version, ordinal, relative_path, sha256)
VALUES ('history', 'fixture', '1', 1, 'migrations/0001_initial.sql', 'sha256:' || repeat('a', 64));
INSERT INTO catalog.package_definition_owners
    (env_key, schema_name, relation_name, definition_type, definition_name, owner_package_id)
VALUES ('history', 'fixture', 'item', 'relation', 'item', 'fixture');
INSERT INTO catalog.releases (env_key, manifest_digest, canonical_bytes)
VALUES ('history', 'sha256:' || encode(sha256('{}'::bytea), 'hex'), '{}'::bytea);
INSERT INTO catalog.connection_requirements
    (env_key, component_digest, store_alias, requirement_json, requirement_hash)
VALUES ('history', 'sha256:' || repeat('a', 64), 'store', '{}', 'sha256:' || repeat('b', 64));
INSERT INTO catalog.connection_instances
    (env_key, environment, instance_id, requirement_type, contract)
VALUES ('history', 'dev', 'store', 'http', 'wamn:connection/http@0.1.0');
INSERT INTO catalog.connection_generations
    (env_key, environment, instance_id, generation, definition_json, definition_hash)
VALUES ('history', 'dev', 'store', 1, '{}', 'sha256:' || repeat('b', 64));
INSERT INTO catalog.connection_bindings
    (env_key, manifest_digest, component_digest, store_alias, environment, instance_id,
     package_id, binding_status, validation_status, validation_hash)
VALUES ('history', 'sha256:' || encode(sha256('{}'::bytea), 'hex'), 'sha256:' || repeat('a', 64),
        'store', 'dev', 'store', 'fixture', 'active', 'valid', 'sha256:' || repeat('b', 64));
INSERT INTO catalog.wiring_tombstones (env_key, package_id, environment, wiring_id, reason)
VALUES ('history', 'fixture', 'dev', 'retired', 'fixture');
INSERT INTO catalog.package_upgrade_qualifications
    (env_key, package_id, candidate_package_version, canonical_bytes, result_sha256,
     predecessor_manifest_digest, package_artifact_digest)
VALUES ('history', 'fixture', '2', '{}'::bytea, 'sha256:' || encode(sha256('{}'::bytea), 'hex'),
        'sha256:' || encode(sha256('{}'::bytea), 'hex'), 'sha256:' || repeat('a', 64));
INSERT INTO catalog.package_upgrade_stages
    (env_key, package_id, package_version, predecessor_version, status, cursor,
     package_artifact_digest, predecessor_release_digest, evidence_digest)
VALUES ('history', 'fixture', '2', '1', 'in_progress', 'null',
        'sha256:' || repeat('a', 64), 'sha256:' || repeat('b', 64), 'sha256:' || repeat('c', 64));
INSERT INTO catalog.package_floors (env_key, package_id, version) VALUES ('history', 'fixture', '1');

DO $assert_inserts$
DECLARE
    relation text;
    stamped boolean;
BEGIN
    FOREACH relation IN ARRAY ARRAY[
        'packages', 'package_migrations', 'package_definition_owners', 'releases',
        'connection_requirements', 'connection_generations', 'connection_bindings',
        'wiring_tombstones', 'package_upgrade_qualifications', 'package_upgrade_stages', 'package_floors'
    ] LOOP
        ASSERT (SELECT count(*) = 4 AND bool_and(attnotnull AND atthasdef = false)
                  FROM pg_attribute
                 WHERE attrelid = format('catalog.%I', relation)::regclass
                   AND attname IN ('created_at', 'created_by', 'updated_at', 'updated_by')),
               format('%s requires four NOT NULL stamps without defaults', relation);
        EXECUTE format(
            'SELECT count(*) > 0 AND bool_and(created_by = %L::uuid AND updated_by = created_by
                 AND created_at = transaction_timestamp() AND updated_at = created_at) FROM catalog.%I',
            '00000000-0000-4000-8000-0000000000f1', relation
        ) INTO stamped;
        ASSERT stamped, format('%s insert stamps', relation);
        PERFORM set_config('app.user_id', '', true);
        BEGIN
            EXECUTE format('INSERT INTO catalog.%I SELECT * FROM catalog.%I', relation, relation);
            RAISE EXCEPTION '% accepted an insert without an actor', relation;
        EXCEPTION WHEN SQLSTATE '55000' THEN
            ASSERT SQLERRM = 'actor-required', format('%s missing actor: %s', relation, SQLERRM);
        END;
        PERFORM set_config('app.user_id', '00000000-0000-4000-8000-0000000000f1', true);
        IF relation NOT IN ('package_upgrade_stages', 'package_floors') THEN
            BEGIN
                EXECUTE format('UPDATE catalog.%I SET created_by = created_by', relation);
                RAISE EXCEPTION '% accepted an immutable update', relation;
            EXCEPTION WHEN SQLSTATE '55000' THEN
                NULL;
            END;
        END IF;
    END LOOP;
END
$assert_inserts$;
COMMIT;

-- transaction boundary
BEGIN;
SELECT set_config('app.user_id', '00000000-0000-4000-8000-0000000000f2', true),
       set_config('app.operation', 'admin:update-catalog-history', true);
UPDATE catalog.package_floors SET version = '2',
    created_by = '00000000-0000-4000-8000-0000000000f2', created_at = '2000-01-01';
UPDATE catalog.package_upgrade_stages SET cursor = '1', completed_batches = 1,
    created_by = '00000000-0000-4000-8000-0000000000f2', created_at = '2000-01-01';
DO $assert_updates$
DECLARE
    relation text;
    stamped boolean;
BEGIN
    FOREACH relation IN ARRAY ARRAY['package_floors', 'package_upgrade_stages'] LOOP
        EXECUTE format(
            'SELECT bool_and(created_by = %L::uuid AND updated_by = %L::uuid
                 AND updated_at = transaction_timestamp() AND created_at < updated_at) FROM catalog.%I',
            '00000000-0000-4000-8000-0000000000f1', '00000000-0000-4000-8000-0000000000f2', relation
        ) INTO stamped;
        ASSERT stamped, format('%s update preserves creation and stamps the editor', relation);
    END LOOP;
END
$assert_updates$;
COMMIT;

-- transaction boundary
BEGIN;
SELECT set_config('app.user_id', '00000000-0000-4000-8000-0000000000f1', true);
DO $assert_noops$
DECLARE
    relation text;
    before_image jsonb;
    after_image jsonb;
BEGIN
    FOREACH relation IN ARRAY ARRAY['package_floors', 'package_upgrade_stages'] LOOP
        EXECUTE format('SELECT to_jsonb(row) FROM catalog.%I row', relation) INTO before_image;
        EXECUTE format('UPDATE catalog.%I SET updated_by = created_by, updated_at = transaction_timestamp()', relation);
        EXECUTE format('SELECT to_jsonb(row) FROM catalog.%I row', relation) INTO after_image;
        ASSERT before_image = after_image, format('%s no-op preserves all stamps', relation);
        PERFORM set_config('app.user_id', '', true);
        BEGIN
            EXECUTE format('UPDATE catalog.%I SET updated_by = updated_by', relation);
            RAISE EXCEPTION '% accepted an update without an actor', relation;
        EXCEPTION WHEN SQLSTATE '55000' THEN
            ASSERT SQLERRM = 'actor-required', format('%s missing update actor', relation);
        END;
        PERFORM set_config('app.user_id', '00000000-0000-4000-8000-0000000000f1', true);
    END LOOP;
END
$assert_noops$;
COMMIT;
