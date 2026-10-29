BEGIN;
SELECT set_config('app.user_id','00000000-0000-4000-8000-000000000001',true),
       set_config('app.operation','admin:connection-schema-test',true);
INSERT INTO platform.integration (id,integration_type,name,status) VALUES
    ('10000000-0000-4000-8000-000000000001','http','http-test','active'),
    ('10000000-0000-4000-8000-000000000002','s3','store-test','active'),
    ('10000000-0000-4000-8000-000000000003','modbus','device-test','active'),
    ('10000000-0000-4000-8000-000000000004','http','http-next','active');
INSERT INTO platform.connection (id,integration_id,integration_type,status) VALUES
    ('20000000-0000-4000-8000-000000000001','10000000-0000-4000-8000-000000000001','http','active'),
    ('20000000-0000-4000-8000-000000000002','10000000-0000-4000-8000-000000000002','s3','active'),
    ('20000000-0000-4000-8000-000000000003','10000000-0000-4000-8000-000000000003','modbus','active'),
    ('20000000-0000-4000-8000-000000000004','10000000-0000-4000-8000-000000000004','http','active');
INSERT INTO catalog.releases (env_key,manifest_digest,canonical_bytes)
VALUES ('test','sha256:' || encode(sha256('{}'::bytea),'hex'),'{}'::bytea);
INSERT INTO catalog.connection_requirement (env_key,component_digest,package_id,name,integration_type,interfaces,mode)
VALUES ('test','sha256:' || repeat('a',64),'fixture','erp','http',ARRAY['wamn:connection/http@0.1.0'],'read');
INSERT INTO catalog.release_connection (env_key,manifest_digest,connection_id,integration_type,version) VALUES
    ('test','sha256:' || encode(sha256('{}'::bytea),'hex'),'20000000-0000-4000-8000-000000000001','http',1),
    ('test','sha256:' || encode(sha256('{}'::bytea),'hex'),'20000000-0000-4000-8000-000000000002','s3',1),
    ('test','sha256:' || encode(sha256('{}'::bytea),'hex'),'20000000-0000-4000-8000-000000000004','http',1);
DO $$ BEGIN
    BEGIN
        INSERT INTO catalog.connection_binding (env_key,manifest_digest,component_digest,package_id,name,connection_id,integration_type)
        VALUES ('test','sha256:' || encode(sha256('{}'::bytea),'hex'),'sha256:' || repeat('a',64),'fixture','erp','20000000-0000-4000-8000-000000000002','s3');
        RAISE EXCEPTION 'binding type mismatch accepted';
    EXCEPTION WHEN foreign_key_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO catalog.connection_binding (env_key,manifest_digest,component_digest,package_id,name,connection_id,integration_type)
        VALUES ('test','sha256:' || encode(sha256('{}'::bytea),'hex'),'sha256:' || repeat('a',64),'fixture','erp','20000000-0000-4000-8000-000000000002','http');
        RAISE EXCEPTION 'release connection type mismatch accepted';
    EXCEPTION WHEN foreign_key_violation THEN NULL;
    END;
END $$;
INSERT INTO catalog.connection_binding (env_key,manifest_digest,component_digest,package_id,name,connection_id,integration_type)
VALUES ('test','sha256:' || encode(sha256('{}'::bytea),'hex'),'sha256:' || repeat('a',64),'fixture','erp','20000000-0000-4000-8000-000000000001','http');
DO $$
DECLARE refused_connection uuid;
BEGIN
    FOREACH refused_connection IN ARRAY ARRAY[
        '20000000-0000-4000-8000-000000000001'::uuid,
        '20000000-0000-4000-8000-000000000003'::uuid
    ] LOOP
        BEGIN
            INSERT INTO platform.file (id,application,connection_id,name,media_type,size,sha256,blob_key)
            VALUES ('30000000-0000-4000-8000-000000000001','fixture',refused_connection,'test','text/plain',0,repeat('a',64),'_wamn_files/live/30000000-0000-4000-8000-000000000001');
            RAISE EXCEPTION 'file accepted a connection without host storage';
        EXCEPTION WHEN check_violation THEN NULL;
        END;
    END LOOP;
END $$;
INSERT INTO platform.file (id,application,connection_id,name,media_type,size,sha256,blob_key)
VALUES ('30000000-0000-4000-8000-000000000001','fixture','20000000-0000-4000-8000-000000000002','test','text/plain',0,repeat('a',64),'_wamn_files/live/30000000-0000-4000-8000-000000000001');
COMMIT;

BEGIN;
SELECT set_config('app.user_id','00000000-0000-4000-8000-000000000002',true),
       set_config('app.operation','admin:update-connection-schema-test',true);
UPDATE catalog.release_connection SET version=2 WHERE integration_type='http';
UPDATE catalog.connection_binding SET name='erp' WHERE name='erp';
DO $$ BEGIN
    ASSERT (SELECT count(*)=1 FROM catalog.connection_binding_history), 'binding no-op logs no history';
    ASSERT (SELECT count(*)=5 FROM catalog.release_connection_history), 'release changes log history';
    ASSERT (SELECT created_by='00000000-0000-4000-8000-000000000001' AND updated_by='00000000-0000-4000-8000-000000000002'
        FROM catalog.release_connection WHERE connection_id='20000000-0000-4000-8000-000000000001'), 'release update preserves creation actor';
END $$;
UPDATE catalog.connection_binding SET connection_id='20000000-0000-4000-8000-000000000004';
DO $$ BEGIN
    ASSERT (SELECT created_by='00000000-0000-4000-8000-000000000001' AND updated_by='00000000-0000-4000-8000-000000000002' FROM catalog.connection_binding), 'binding update preserves creation actor';
END $$;
DELETE FROM catalog.connection_binding;
DELETE FROM catalog.release_connection;
DO $$ BEGIN
    ASSERT (SELECT array_agg(type ORDER BY position)=ARRAY['insert','update','delete'] FROM catalog.connection_binding_history), 'binding insert and delete log history';
    ASSERT (SELECT array_agg(type ORDER BY position)=ARRAY['insert','insert','insert','update','update','delete','delete','delete'] FROM catalog.release_connection_history), 'release insert update and delete log history';
    ASSERT (SELECT bool_and(changed_by='00000000-0000-4000-8000-000000000002') FROM catalog.connection_binding_history WHERE type='delete'), 'binding deletion logs actor';
END $$;
COMMIT;
