BEGIN;
SELECT set_config('app.user_id', '00000000-0000-4000-8000-0000000000f1', true),
       set_config('app.operation', 'admin:seed-catalog-history', true);
INSERT INTO platform."user" (id, type, email, display_name) VALUES
    ('00000000-0000-4000-8000-0000000000f1', 'user', 'creator@example.invalid', 'creator'),
    ('00000000-0000-4000-8000-0000000000f2', 'user', 'editor@example.invalid', 'editor');
INSERT INTO catalog.packages (physical_schema_fingerprint, env_key, package_id, package_version)
VALUES ('sha256:' || repeat('c', 64), 'history', 'fixture', '1');
INSERT INTO catalog.package_migrations
    (env_key, package_id, package_version, ordinal, relative_path, sha256)
VALUES ('history', 'fixture', '1', 1, 'migrations/0001_initial.sql', 'sha256:' || repeat('a', 64));
INSERT INTO catalog.package_definition_owners
    (env_key, schema_name, relation_name, definition_type, definition_name, owner_package_id)
VALUES ('history', 'fixture', 'item', 'relation', 'item', 'fixture');
INSERT INTO catalog.releases (env_key, manifest_digest, canonical_bytes)
VALUES ('history', 'sha256:' || encode(sha256('{}'::bytea), 'hex'), '{}'::bytea);
INSERT INTO platform.integration (id,integration_type,name,status)
VALUES ('10000000-0000-4000-8000-000000000001','http','history-http','active');
INSERT INTO platform.connection (id,integration_id,integration_type,status)
VALUES ('10000000-0000-4000-8000-000000000002','10000000-0000-4000-8000-000000000001','http','active');
INSERT INTO catalog.connection_requirement (env_key,component_digest,package_id,name,interface,mode)
VALUES ('history','sha256:' || repeat('a',64),'fixture','store','wamn:connection/http@0.1.0','write');
INSERT INTO catalog.release_connection (env_key,manifest_digest,connection_id,version)
VALUES ('history','sha256:' || encode(sha256('{}'::bytea),'hex'),'10000000-0000-4000-8000-000000000002',1);
INSERT INTO catalog.connection_binding (env_key,manifest_digest,component_digest,package_id,name,connection_id)
VALUES ('history','sha256:' || encode(sha256('{}'::bytea),'hex'),'sha256:' || repeat('a',64),'fixture','store','10000000-0000-4000-8000-000000000002');
INSERT INTO catalog.wiring_tombstones (env_key, package_id, environment, wiring_id, reason)
VALUES ('history', 'fixture', 'dev', 'retired', 'fixture');
DO $assert_inserts$
DECLARE
    relation text;
    stamped boolean;
BEGIN
    FOREACH relation IN ARRAY ARRAY[
        'packages', 'package_migrations', 'package_definition_owners', 'releases',
        'release_connection', 'connection_binding',
        'wiring_tombstones'
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
        IF relation NOT IN ('packages', 'release_connection', 'connection_binding') THEN
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
