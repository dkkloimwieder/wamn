//! External signature authentication uses only the connection principal's grants.
use super::*;
use crate::plugins::wamn_postgres::{
    AuthorityClass, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};
use wamn_broker::auth_changes::AuthDrops;
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, sql, workload_login};

use sqlx::Connection as _;
async fn connect(url: &str) -> sqlx::PgConnection {
    let client = wamn_postgres::connect(url).await.unwrap();

    client
}

#[tokio::test]
async fn external_signature_uses_the_service_principal_and_only_admin_granted_roles() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = crate::connection_credentials::test_util::Fixture::new().await;
    fixture
        .put(
            "webhook",
            r#"{"hmac":{"algorithm":"sha256","secret":"test-secret"}}"#,
            &["host-project"],
        )
        .await;
    let principal: String = sqlx::query("SELECT c.service_principal_id::text FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id WHERE i.name='webhook' AND c.project='host-project'").fetch_one(&mut fixture.admin).await.unwrap().get(0);
    let database = wamn_test_postgres::database();
    let mut project = connect(database.url()).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (sql::ensure_db_owner_role_sql()).to_string(),
    ))
    .execute(&mut project)
    .await
    .map(drop)
    .unwrap();
    sqlx::raw_sql("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN CREATE ROLE wamn_app NOLOGIN NOINHERIT; END IF; END $$; DO $$ BEGIN  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_http_admitter') THEN CREATE ROLE wamn_http_admitter NOLOGIN NOINHERIT; END IF; END $$; CREATE SCHEMA wamn_run;").execute(&mut project).await.map(drop).unwrap();
    sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
        .execute(&mut project)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(wamn_control_provision::APP_SCHEMA_SQL)
        .execute(&mut project)
        .await
        .map(drop)
        .unwrap();
    let role = workload_login(
        WorkloadRoleFamily::HttpAdmitter,
        WorkloadRoleIdentity::ProjectEnvironment {
            org: "acme",
            project: "host-project",
            environment: "prod",
            database: database.name(),
        },
    )
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::HttpAdmitter,
            database.name(),
            &role,
            "fixture-password",
        ))
            .to_string(),
    ))
    .execute(&mut project)
    .await
    .map(drop)
    .unwrap();
    sqlx::query("SELECT set_config('app.user_id',$1,false),set_config('app.operation','admin:connection-principal',false)").bind(&principal).execute(&mut project).await.map(|result| result.rows_affected()).unwrap();
    sqlx::query("INSERT INTO platform.\"user\"(id,type,email) VALUES ($1::text::uuid,'service','webhook@example.test')").bind(&principal).execute(&mut project).await.map(|result| result.rows_affected()).unwrap();
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
    let mut postgres =
        WamnPostgres::with_provider(Arc::new(StaticCredentialProvider::new(sources, None)));
    let applications = ["receiver"];
    assert!(
        postgres
            .configure_revocation("host-project", &applications)
            .await
            .is_err()
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("INSERT INTO platform.setting(target_type,target_id,key,value) VALUES \
        ('project','host-project','revocation-seconds','60'), \
        ('application','receiver','revocation-seconds','5')")
            .to_string(),
    ))
    .execute(&mut project)
    .await
    .unwrap();
    postgres
        .configure_revocation("host-project", &applications)
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("UPDATE platform.setting SET value='true' WHERE target_type='application'").to_string(),
    ))
    .execute(&mut project)
    .await
    .unwrap();
    assert!(
        postgres
            .configure_revocation("host-project", &applications)
            .await
            .is_err()
    );
    let postgres = Arc::new(postgres);
    let facts = Arc::new(crate::connection_facts::ConnectionFacts::new(
        Arc::new(crate::connection_memory::ConnectionMemory::default()),
        wamn_broker::Publisher::default(),
        Arc::new(fixture.reader("host-project").await),
        &postgres,
        "acme--dev--host-project",
        "host-project",
        "dev",
    ));
    let authority = wamn_postgres::pool(fixture.database.url(), 4)
        .await
        .unwrap();
    let authentication = Arc::new(PlatformRouteAuthenticator::new(
        Arc::clone(&postgres),
        [facts],
        Some((authority, Default::default())),
    ));
    let routing = routing_policy(
        json!({"modes":["signature"],"header":"X-Signature","credential":"org:acme:webhook@2"}),
    )
    .with_authenticator(Arc::clone(&authentication) as _);
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
    sqlx::raw_sql("INSERT INTO platform.role(name) VALUES ('source-reader'); INSERT INTO platform.permission(role_name,permission) VALUES ('source-reader','fixture:item/read')").execute(&mut project).await.map(drop).unwrap();
    sqlx::query(
        "INSERT INTO platform.user_role(user_id,role_name) VALUES ($1::text::uuid,'source-reader')",
    )
    .bind(&principal)
    .execute(&mut project)
    .await
    .map(|result| result.rows_affected())
    .unwrap();
    postgres.auth_cache().clear();
    let caller = routing
        .authenticate_request_for_test("orders", &headers, super::signature::BODY)
        .await
        .unwrap()
        .unwrap();
    assert!(caller.permits("fixture:item/read@1.0.0"));
    assert!(!caller.permits("fixture:item/write@1.0.0"));
    assert!(caller.active_role().is_none());
    // An API service needs its application grants, with no operator identity or system admin role.
    let mut authority = connect(fixture.database.url()).await;
    let mut tx = authority.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL ROLE wamn_system")
        .execute(&mut *tx)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut *tx)
    .await
    .map(drop)
    .unwrap();
    let service = wamn_platform_identity::create_service(
        &mut tx,
        "api.acme.host-project.reader",
        "API reader",
        &mut AuthDrops::default(),
    )
    .await
    .unwrap();
    let pat = wamn_platform_identity::issue_pat(
        &mut tx,
        service.id(),
        "API reader",
        std::time::Duration::from_secs(3600),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("INSERT INTO platform.\"user\"(id,type,email) VALUES ($1::text::uuid,'service','api@example.test')").bind(service.id().to_string()).execute(&mut project).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(("INSERT INTO platform.user_role(user_id,role_name) VALUES ($1::text::uuid,'source-reader')").to_string())).bind(&service.id().to_string()).execute(&mut project).await.unwrap();
    let bearer = format!("Bearer {}", pat.token());
    let pat_routing = super::routing(&json!(["pat"])).with_authenticator(authentication);
    let api = pat_routing
        .authenticate_headers_for_test("orders", &[("Authorization", &bearer)])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(api.principal_id(), service.id().to_string());
    assert!(!api.admin());
    assert!(api.permits("fixture:item/read@1.0.0"));
    assert!(!api.permits("fixture:item/write@1.0.0"));
    sqlx::query("UPDATE platform.\"user\" SET status='disabled' WHERE id=$1::text::uuid")
        .bind(&principal)
        .execute(&mut project)
        .await
        .map(|result| result.rows_affected())
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
    sqlx::query("UPDATE platform.\"user\" SET status='active' WHERE id=$1::text::uuid")
        .bind(&principal)
        .execute(&mut project)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    sqlx::query("UPDATE platform.\"user\" SET type='user' WHERE id=$1::text::uuid")
        .bind(&principal)
        .execute(&mut project)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    postgres.auth_cache().clear();
    assert!(
        routing
            .authenticate_request_for_test("orders", &headers, super::signature::BODY)
            .await
            .is_err()
    );
}
