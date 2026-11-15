//! Registry SQL used by database boundary tests.

/// Database fixture statement for apply_authority.
pub fn apply_authority_sql() -> &'static str {
    r"
        DO $denials$ DECLARE relation text; privilege text; BEGIN
            ASSERT (SELECT rolcanlogin AND NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole AND NOT rolreplication AND NOT rolbypassrls FROM pg_roles WHERE rolname='wamn_apply');
            ASSERT pg_has_role('wamn_apply', 'wamn_db_owner', 'MEMBER');
            ASSERT NOT pg_has_role('wamn_apply', 'wamn_system', 'MEMBER');
            FOREACH relation IN ARRAY ARRAY[
                'registry.connection', 'registry.integration', 'registry.connection_generation',
                'identity.credential', 'identity.oauth_token', 'identity.oauth_authorization', 'identity.pats'
            ] LOOP
                FOREACH privilege IN ARRAY ARRAY['INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER'] LOOP
                    ASSERT NOT has_table_privilege('wamn_apply', relation, privilege), relation || ':' || privilege;
                END LOOP;
                ASSERT NOT has_any_column_privilege('wamn_apply', relation, 'INSERT,UPDATE,REFERENCES'), relation;
            END LOOP;
        END $denials$;
        SET SESSION AUTHORIZATION wamn_apply;
        DO $role_denial$ BEGIN
            BEGIN
                SET ROLE wamn_system;
                RAISE EXCEPTION 'apply assumed the system owner';
            EXCEPTION WHEN insufficient_privilege THEN NULL; END;
            ASSERT current_user = 'wamn_apply';
        END $role_denial$;
        SELECT registry.connection_projection('absent','absent','dev');
        SELECT principal_id FROM identity.org_memberships ORDER BY principal_id FOR UPDATE;
        SELECT * FROM registry.resolve_environment_connection('absent','absent','dev','cloud','absent',1);
        RESET SESSION AUTHORIZATION;
    "
}

/// Database fixture statement for projection.
pub fn seed_connection_projection_sql() -> &'static str {
    r"
            UPDATE registry.meta SET platform_domain='example.invalid';
            UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='cloud');
            INSERT INTO registry.external_system(org,project,name) VALUES ('acme','project-a','source'),('acme','project-b','source');
            INSERT INTO registry.integration(org,project,name,integration_type,auth_type,status)
            VALUES ('acme','project-a','disabled','http','none','disabled'),
                   ('acme','project-a','active','http','none','active'),
                   ('acme','project-b','active','http','none','active');
            INSERT INTO identity.principals(id,type,subject,display_name)
            SELECT gen_random_uuid(),'service','projection-' || row_number() OVER()::text,'projection' FROM registry.environment_projects WHERE org='acme';
            INSERT INTO registry.connection(integration_id,org,project,env,integration_type,target_id,active_generation,service_principal_id)
            SELECT i.id,i.org,i.project,e.env,i.integration_type,v.target_id,1,p.id
            FROM registry.integration i JOIN registry.environment_projects e ON (e.org,e.project)=(i.org,i.project) JOIN registry.environments v ON (v.org,v.env)=(e.org,e.env)
            JOIN LATERAL (SELECT id FROM identity.principals WHERE subject='projection-' ||
                CASE WHEN i.project='project-b' THEN '3' WHEN e.env='prod' THEN '2' ELSE '1' END) p ON true
            WHERE i.name=CASE WHEN i.project='project-a' THEN 'disabled' ELSE 'active' END;
            INSERT INTO registry.connection_generation(connection_id,generation,created_at,created_by)
            SELECT id,1,now(),'00000000-0000-4000-8000-000000000001' FROM registry.connection;
        "
}

/// Database fixture statement for projection.
pub fn disable_project_connection_sql() -> &'static str {
    "SET LOCAL ROLE wamn_system; UPDATE registry.connection SET status='disabled' WHERE org='acme' AND project='project-b'; SET LOCAL ROLE wamn_control"
}

/// Invalid writes refused by the applied configuration schema.
pub fn invalid_configuration_sql() -> &'static [&'static str] {
    &[
        "UPDATE registry.environment_projects SET revocation_seconds=-1 WHERE org='acme'",
        "UPDATE registry.environment_projects SET revocation_seconds=3601 WHERE org='acme'",
        "UPDATE registry.environment_projects SET schema_mode='unknown' WHERE org='acme'",
        "UPDATE registry.environment_applications SET revocation_seconds=NULL WHERE org='acme'",
        "UPDATE registry.environment_applications SET revocation_seconds=-1 WHERE org='acme'",
        "UPDATE registry.environment_applications SET revocation_seconds=3601 WHERE org='acme'",
        "UPDATE registry.environment_application_connections SET connection_name='shared-erp@1' WHERE org='acme'",
        "UPDATE registry.environment_applications SET client_web=NULL WHERE org='acme'",
        "UPDATE registry.environment_applications SET project='missing' WHERE org='acme'",
        "UPDATE registry.environment_application_connections SET application='missing' WHERE org='acme'",
        "UPDATE registry.environment_integrations SET project='missing' WHERE org='acme'",
        "INSERT INTO registry.environment_applications SELECT * FROM registry.environment_applications WHERE org='acme'",
        "INSERT INTO registry.environment_application_connections SELECT * FROM registry.environment_application_connections WHERE org='acme'",
        "INSERT INTO registry.environment_integrations SELECT * FROM registry.environment_integrations WHERE org='acme'",
    ]
}

/// Database fixture statement for credentials.
pub fn connection_principal_sql() -> &'static str {
    "SELECT service_principal_id FROM registry.connection WHERE id=$1"
}

/// Database fixture statement for credentials.
pub fn disable_project_integrations_sql() -> &'static str {
    "SET ROLE wamn_system; UPDATE registry.integration SET status='disabled' WHERE project='project-a'; RESET ROLE"
}

/// Database fixture statement for credentials.
pub fn disable_files_integration_sql() -> &'static str {
    "SET ROLE wamn_system; UPDATE registry.integration SET status='disabled' WHERE project='project-a' AND name='files'; RESET ROLE"
}

/// Database fixture statement for credentials.
pub fn hmac_http_only_sql() -> &'static str {
    "SELECT http_only FROM registry.auth_type WHERE code='hmac'"
}

/// Database fixture statement for provision.
pub fn seed_disposable_policy_sql() -> &'static str {
    "INSERT INTO registry.orgs (id,placement_type) VALUES ('demo','dedicated'); INSERT INTO registry.env_policies (org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) VALUES ('demo','dev','\"own\"',0,1,'1Gi','1','1Gi','postgres:18');"
}

/// Database fixture statement for provision.
pub fn disposable_environment_sql() -> &'static str {
    "SELECT disposable FROM registry.environments WHERE org='demo' AND env='dev'"
}

/// Database fixture statement for pat.
pub fn seed_pat_project_sql() -> &'static str {
    "INSERT INTO registry.orgs (id, placement_type, pool_cluster) VALUES ('org', 'pooled', 'test'); INSERT INTO registry.projects (org, id) VALUES ('org', 'project')"
}

/// Seed the credential fixture's authentication and deployment target.
pub fn seed_credential_target_sql() -> &'static str {
    "INSERT INTO registry.integration_type_auth VALUES ('http','custom'); INSERT INTO registry.target(name,type,status) VALUES ('fixture','local','active'); UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='fixture'); RESET ROLE"
}

/// Seed the application activation target.
pub fn activation_target_sql() -> &'static str {
    "INSERT INTO registry.target(name,type,status) VALUES('cloud','cloud','active') ON CONFLICT DO NOTHING; UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='cloud');"
}

/// Registry statement used by connection or deletion boundary tests.
pub fn deployment_rows_sql() -> &'static str {
    "SELECT to_jsonb(e) FROM registry.environment_projects e"
}

/// Registry statement used by connection or deletion boundary tests.
pub fn deletion_target_sql() -> &'static str {
    "INSERT INTO registry.target(name,type,status) VALUES ('delete-fixture','local','active'); \
         UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='delete-fixture')"
}

/// Registry statement used by connection or deletion boundary tests.
pub fn deleted_deployment_sql() -> &'static str {
    "SELECT 1 FROM registry.environment_projects WHERE org='acme' AND project='app' AND env='dev'"
}

/// Registry statement used by connection or deletion boundary tests.
pub fn deleted_connections_sql() -> &'static str {
    "SELECT 1 FROM registry.connection WHERE org='acme' AND project='app' AND env='dev'"
}

/// Registry statement used by connection or deletion boundary tests.
pub fn deployment_exists_sql() -> &'static str {
    "SELECT 1 FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3"
}

/// Registry statement used by connection or deletion boundary tests.
pub fn connection_fact_fixture_id_sql() -> &'static str {
    "SELECT id FROM registry.connection WHERE project='project-a'"
}

/// Registry statement used by connection or deletion boundary tests.
pub fn revoke_connection_fact_sql() -> &'static str {
    "REVOKE EXECUTE ON FUNCTION registry.read_connection_fact(text,text,text,uuid) FROM wamn_conn_reader"
}

/// Registry statement used by connection or deletion boundary tests.
pub fn grant_connection_fact_sql() -> &'static str {
    "GRANT EXECUTE ON FUNCTION registry.read_connection_fact(text,text,text,uuid) TO wamn_conn_reader"
}
