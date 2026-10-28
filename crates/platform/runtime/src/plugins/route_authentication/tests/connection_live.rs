//! External signature authentication uses only the connection principal's grants.
use super::*;
use crate::plugins::wamn_postgres::{
    AuthorityClass, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};
use wamn_platform_identity::{
    auth_changes::AuthDrops,
    connections::{self, ConnectionWrite},
};

async fn connect(url: &str) -> tokio_postgres::Client {
    let (client, driver) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(driver);
    client
}

#[tokio::test]
async fn external_signature_uses_the_service_principal_and_only_admin_granted_roles() {
    let _lock = wamn_test_postgres::lock();
    let system = wamn_control_provision::test_database::system();
    let mut authority = connect(system.url()).await;
    authority
        .batch_execute("SET ROLE wamn_system")
        .await
        .unwrap();
    let tx = authority.transaction().await.unwrap();
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
    tx.batch_execute("INSERT INTO registry.orgs(id,placement_type) VALUES ('acme','dedicated'); INSERT INTO registry.projects(org,id) VALUES ('acme','host-project')").await.unwrap();
    tx.batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
    let name = connections::write_connection(&tx,"acme",&ConnectionWrite {
        integration:"http".into(), external_system:None, name:"webhook".into(), transport:"http".into(), exchange:"request".into(),
        parameters: json!({"primary-authority":"https://source.example","tls-verification":"verify-authority"}),status:"active".into()
    },&mut AuthDrops::default()).await.unwrap();
    connections::enable_connection(&tx, "acme", "host-project", &name)
        .await
        .unwrap();
    connections::rotate(
        &tx,
        "acme",
        &name,
        r#"{"hmac":{"algorithm":"sha256","secret":"test-secret"}}"#,
    )
    .await
    .unwrap();
    let principal: String = tx
        .query_one(
            "SELECT service_principal_id::text FROM registry.connection_rows($1) WHERE name=$2",
            &[&"acme", &name],
        )
        .await
        .unwrap()
        .get(0);
    tx.commit().await.unwrap();

    let database = wamn_test_postgres::database();
    let project = connect(database.url()).await;
    project
        .batch_execute(sql::ensure_db_owner_role_sql())
        .await
        .unwrap();
    project.batch_execute("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN CREATE ROLE wamn_app NOLOGIN; END IF; END $$; DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_scenario_author') THEN CREATE ROLE wamn_scenario_author NOLOGIN; END IF; IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_http_admitter') THEN CREATE ROLE wamn_http_admitter NOLOGIN; END IF; END $$; CREATE SCHEMA wamn_run;").await.unwrap();
    project
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await
        .unwrap();
    project
        .batch_execute(wamn_control_provision::APP_SCHEMA_SQL)
        .await
        .unwrap();
    let role = workload_generation_role(
        WorkloadRoleFamily::HttpAdmitter,
        WorkloadRoleScope::ProjectEnvironment {
            org: "acme",
            project: "host-project",
            environment: "prod",
            database: database.name(),
        },
        CredentialGeneration::A,
    )
    .unwrap();
    project
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::HttpAdmitter,
            database.name(),
            &role,
            "fixture-password",
            "2099-01-01T00:00:00Z",
        ))
        .await
        .unwrap();
    project.execute("SELECT set_config('app.user_id',$1,false),set_config('app.operation','admin:connection-principal',false)",&[&principal]).await.unwrap();
    project.execute("INSERT INTO platform.\"user\"(id,type,email) VALUES ($1::text::uuid,'service','webhook@example.test')",&[&principal]).await.unwrap();
    let mut reader = url::Url::parse(database.url()).unwrap();
    reader.set_username(&role).unwrap();
    reader.set_password(Some("fixture-password")).unwrap();
    let base = WamnPostgresConfig {
        credentials: None,
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2000,
        statement_timeout_ms: 5000,
        row_limit: 100,
    };
    let sources = StaticCredentialProvider::projects_from_json(&json!({"host-project":{"credentials":{(AuthorityClass::CallableHttp.as_str()):reader.to_string()}}}).to_string(),&base).unwrap();
    let postgres = Arc::new(WamnPostgres::with_provider(Arc::new(
        StaticCredentialProvider::new(sources, None),
    )));
    authority.batch_execute("RESET ROLE").await.unwrap();
    authority
        .batch_execute(&sql::grant_connection_reader_surface_sql())
        .await
        .unwrap();
    let credential_role = format!("c35_signature_{}", system.name());
    authority
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::ConnectionReader,
            system.name(),
            &credential_role,
            "fixture-password",
            "2099-01-01T00:00:00Z",
        ))
        .await
        .unwrap();
    let tx = authority.transaction().await.unwrap();
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
    tx.execute(
        sql::upsert_control_author_env_key_mapping_sql(),
        &[
            &credential_role,
            &"acme--host-project--prod",
            &"acme",
            &"host-project",
            &"prod",
        ],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut credential_url = url::Url::parse(system.url()).unwrap();
    credential_url.set_username(&credential_role).unwrap();
    credential_url
        .set_password(Some("fixture-password"))
        .unwrap();
    let credential_reader = connect(credential_url.as_str()).await;
    let credentials = Arc::new(WamnCredentials::from_database(
        Arc::new(credential_reader),
        "acme",
    ));
    let authority = Arc::new(authority);
    let authentication = Arc::new(
        RouteAuthentication::new(
            authority,
            Arc::clone(&postgres),
            "acme",
            "host-project",
            "unused",
        )
        .await
        .unwrap(),
    );
    let routing = routing_policy(
        json!({"modes":["signature"],"header":"X-Signature","credential":"org:acme:webhook@2"}),
    )
    .with_authenticator(Arc::new(
        PlatformRouteAuthenticator::default()
            .with_authentication(Arc::clone(&authentication))
            .with_signature_authentication(credentials, "host-project"),
    ));
    let signature = super::signature::signed(super::signature::BODY);
    let headers = [("X-Signature", signature.as_str())];
    let caller = routing
        .authenticate_request_for_test("orders", &headers, super::signature::BODY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(caller.principal_id(), principal);
    assert_eq!(caller.credential_type(), CredentialType::Connection);
    assert!(!caller.admin());
    assert!(caller.active_role().is_none());
    assert!(!caller.permits("fixture:item/read@1.0.0"));
    project.batch_execute("INSERT INTO platform.role(name) VALUES ('source-reader'); INSERT INTO platform.permission(role_name,permission) VALUES ('source-reader','fixture:item/read')").await.unwrap();
    project.execute("INSERT INTO platform.user_role(user_id,role_name) VALUES ($1::text::uuid,'source-reader')",&[&principal]).await.unwrap();
    postgres.auth_cache().clear();
    let caller = routing
        .authenticate_request_for_test("orders", &headers, super::signature::BODY)
        .await
        .unwrap()
        .unwrap();
    assert!(caller.permits("fixture:item/read@1.0.0"));
    assert!(!caller.permits("fixture:item/write@1.0.0"));
    assert!(caller.active_role().is_none());
    let mut authority = connect(system.url()).await;
    let tx = authority.transaction().await.unwrap();
    tx.batch_execute("SET LOCAL ROLE wamn_system")
        .await
        .unwrap();
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
    let service = wamn_platform_identity::create_service(
        &tx,
        "wrong-subject",
        "Wrong subject",
        &mut AuthDrops::default(),
    )
    .await
    .unwrap();
    wamn_platform_identity::assign_project_role(
        &tx,
        service.id(),
        "acme",
        "host-project",
        "admin",
        &mut AuthDrops::default(),
    )
    .await
    .unwrap();
    let pat = wamn_platform_identity::issue_pat(
        &tx,
        service.id(),
        "wrong subject",
        std::time::Duration::from_secs(3600),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let authenticated = authentication
        .prepared
        .authenticate_route_pat(
            authentication.identity_reader.as_ref(),
            pat.token(),
            "acme",
            "host-project",
            "prod",
            &[ADMIN_ROLE],
        )
        .await
        .unwrap()
        .expect("the PAT is valid for this project");
    assert_eq!(authenticated.principal().id(), service.id());
    let bearer = format!("Bearer {}", pat.token());
    let pat_routing = super::routing(&json!(["pat"])).with_authenticator(Arc::new(
        PlatformRouteAuthenticator::default().with_authentication(authentication),
    ));
    assert_eq!(
        pat_routing
            .authenticate_headers_for_test("orders", &[("Authorization", &bearer)])
            .await
            .unwrap_err()
            .0,
        401
    );
    project
        .execute(
            "UPDATE platform.\"user\" SET status='disabled' WHERE id=$1::text::uuid",
            &[&principal],
        )
        .await
        .unwrap();
    postgres.auth_cache().clear();
    assert_eq!(
        routing
            .authenticate_request_for_test("orders", &headers, super::signature::BODY)
            .await
            .unwrap_err()
            .0,
        401
    );
    project
        .execute(
            "UPDATE platform.\"user\" SET status='active' WHERE id=$1::text::uuid",
            &[&principal],
        )
        .await
        .unwrap();
    project
        .execute(
            "UPDATE platform.\"user\" SET type='user' WHERE id=$1::text::uuid",
            &[&principal],
        )
        .await
        .unwrap();
    postgres.auth_cache().clear();
    assert!(
        routing
            .authenticate_request_for_test("orders", &headers, super::signature::BODY)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn queued_run_without_a_selected_role_uses_only_active_services_and_all_their_roles() {
    const SERVICE: &str = "5345555a-335c-49e4-917b-a187b134cf8c";
    const USER: &str = "c6d14132-207e-4c8b-a8f3-2ad242c10dc1";
    const ABSENT: &str = "13e42097-1f19-4e5b-a79a-5c333c57f677";
    let _lock = wamn_test_postgres::lock();
    let database = wamn_project_state::test_database::tenant_platform();
    let client = connect(database.url()).await;
    client.execute("SELECT set_config('app.user_id',$1,false),set_config('app.operation','admin:queued-caller-fixture',false)", &[&SERVICE]).await.unwrap();
    client.execute("INSERT INTO platform.\"user\"(id,type,email) VALUES ($1::text::uuid,'service','queued-service@example.test'),($2::text::uuid,'user','queued-user@example.test')", &[&SERVICE, &USER]).await.unwrap();
    client.batch_execute("INSERT INTO platform.role(name) VALUES ('reader'),('writer'); INSERT INTO platform.permission(role_name,permission) VALUES ('reader','fixture:item/read'),('writer','fixture:item/write')").await.unwrap();
    client.execute("INSERT INTO platform.user_role(user_id,role_name) VALUES ($1::text::uuid,'reader'),($1::text::uuid,'writer')", &[&SERVICE]).await.unwrap();
    let caller = queued_run_caller(&client, SERVICE, None).await.unwrap();
    assert_eq!(caller.principal_id(), SERVICE);
    assert_eq!(caller.credential_type(), CredentialType::QueuedService);
    assert!(caller.permits("fixture:item/read@1.0.0"));
    assert!(caller.permits("fixture:item/write@1.0.0"));
    assert!(!caller.permits("fixture:item/delete@1.0.0"));
    assert!(!caller.admin());
    assert!(caller.active_role().is_none());
    assert_eq!(
        queued_run_caller(&client, USER, None)
            .await
            .unwrap_err()
            .to_string(),
        "queued service principal is absent or inactive"
    );
    assert_eq!(
        queued_run_caller(&client, ABSENT, None)
            .await
            .unwrap_err()
            .to_string(),
        "queued principal is absent or inactive"
    );
    client
        .execute(
            "UPDATE platform.\"user\" SET status='disabled' WHERE id=$1::text::uuid",
            &[&SERVICE],
        )
        .await
        .unwrap();
    assert_eq!(
        queued_run_caller(&client, SERVICE, None)
            .await
            .unwrap_err()
            .to_string(),
        "queued principal is absent or inactive"
    );
}
