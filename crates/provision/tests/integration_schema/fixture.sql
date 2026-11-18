INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES('00000000-0000-0000-0000-000000000001'::text::uuid,'','fixture','admin:fixture');
INSERT INTO registry.orgs (id) VALUES ('acme');
INSERT INTO registry.projects (org, id) VALUES ('acme', 'app');
INSERT INTO registry.env_policies
    (org, name)
VALUES ('acme', 'dev'), ('acme', 'prod');
INSERT INTO registry.target (id, name, type, status)
VALUES ('00000000-0000-0000-0000-000000000051', 'first', 'local', 'active'),
       ('00000000-0000-0000-0000-000000000052', 'second', 'local', 'active');
WITH parent_environment AS (INSERT INTO registry.environments (org,env,policy_name,target_id) VALUES ('acme','dev','dev','00000000-0000-0000-0000-000000000051'),('acme','prod','prod',NULL) ON CONFLICT (org,env) DO NOTHING) INSERT INTO registry.environment_projects (org,project,env,secret_name,instance_suffix) VALUES ('acme','app','dev','test-dev','aaaaaaa1'),('acme','app','prod','test-prod','aaaaaaa2');
INSERT INTO identity.principals (id, type, subject, display_name)
VALUES ('00000000-0000-0000-0000-000000000001', 'service', 'first', 'First'),
       ('00000000-0000-0000-0000-000000000002', 'service', 'second', 'Second'),
       ('00000000-0000-0000-0000-000000000003', 'service', 'third', 'Third');
SELECT registry.configure_external_system('acme', 'external_system.write', '{"project":"app","name":"erp"}');
INSERT INTO registry.integration (id, org, project, name, integration_type, auth_type, external_system_id)
SELECT '00000000-0000-0000-0000-000000000021', 'acme', 'app', 'first', 'http', 'http_basic', id
FROM registry.external_system WHERE name='erp';
INSERT INTO registry.integration (id, org, project, name, integration_type, auth_type)
VALUES ('00000000-0000-0000-0000-000000000022', 'acme', 'app', 'second', 'http', 'http_basic');
BEGIN;
INSERT INTO registry.connection
    (id, integration_id, org, project, env, integration_type, target_id, active_generation, service_principal_id)
VALUES ('00000000-0000-0000-0000-000000000031', '00000000-0000-0000-0000-000000000021',
        'acme', 'app', 'dev', 'http', '00000000-0000-0000-0000-000000000051', 2, '00000000-0000-0000-0000-000000000001'),
       ('00000000-0000-0000-0000-000000000032', '00000000-0000-0000-0000-000000000022',
        'acme', 'app', 'dev', 'http', '00000000-0000-0000-0000-000000000051', 1, '00000000-0000-0000-0000-000000000002');
INSERT INTO identity.credential (id, connection_id, store, ref, version, created_at, created_by)
VALUES ('00000000-0000-0000-0000-000000000041', '00000000-0000-0000-0000-000000000031',
        'k8s', 'first', '1', now(), '00000000-0000-0000-0000-000000000001'),
       ('00000000-0000-0000-0000-000000000042', '00000000-0000-0000-0000-000000000032',
        'k8s', 'second', '1', now(), '00000000-0000-0000-0000-000000000001');
INSERT INTO registry.connection_generation (connection_id, generation, parameters, credential_id, created_at, created_by)
VALUES ('00000000-0000-0000-0000-000000000031', 1, '{}', '00000000-0000-0000-0000-000000000041', now(), '00000000-0000-0000-0000-000000000001'),
       ('00000000-0000-0000-0000-000000000031', 2, '{}', '00000000-0000-0000-0000-000000000041', now(), '00000000-0000-0000-0000-000000000001'),
       ('00000000-0000-0000-0000-000000000032', 1, '{}', '00000000-0000-0000-0000-000000000042', now(), '00000000-0000-0000-0000-000000000001'),
       ('00000000-0000-0000-0000-000000000032', 3, '{}', '00000000-0000-0000-0000-000000000042', now(), '00000000-0000-0000-0000-000000000001');
COMMIT;
