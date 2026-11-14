//! External signature authentication uses only the connection principal's grants.
use super::*;
use wamn_broker::auth_changes::AuthDrops;
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, sql, workload_login};
use wamn_postgres_capability::{
    AuthorityClass, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};

use sqlx::Connection as _;
async fn connect(url: &str) -> sqlx::PgConnection {
    let client = wamn_postgres::connect(url).await.unwrap();

    client
}

#[tokio::test]
async fn external_signature_uses_the_service_principal_and_only_admin_granted_roles() {
    let _lock = wamn_test_postgres::lock();
    let mut server = wamn_test_postgres::start(&[]).unwrap();
    let control = server.create_database("control").await.unwrap();
    let mut fixture = wamn_connections::connection_credentials::test_util::Fixture::new(
        url::Url::parse(control.url()).unwrap(),
    )
    .await;
    fixture
        .put(
            "webhook",
            r#"{"hmac":{"algorithm":"sha256","secret":"test-secret"}}"#,
            &["host-project"],
        )
        .await;
    let principal: String = sqlx::query("SELECT c.service_principal_id::text FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id WHERE i.name='webhook' AND c.project='host-project'").fetch_one(&mut fixture.admin).await.unwrap().get(0);
    let database = url::Url::parse(server.create_database("project").await.unwrap().url()).unwrap();
    let mut project = connect(database.as_str()).await;
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
            database: database.path().trim_start_matches('/'),
        },
    )
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::HttpAdmitter,
            database.path().trim_start_matches('/'),
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
    let mut reader = url::Url::parse(database.as_str()).unwrap();
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
    let facts = Arc::new(wamn_connections::connection_facts::ConnectionFacts::new(
        Arc::new(wamn_connections::connection_memory::ConnectionMemory::default()),
        wamn_broker::Publisher::default(),
        Arc::new(fixture.reader("host-project").await),
        &postgres,
        "acme--dev--host-project",
        "host-project",
        "dev",
    ));
    let authority = wamn_postgres::pool(fixture.database.as_str(), 4)
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
    let mut authority = connect(fixture.database.as_str()).await;
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

/// A cached PAT remains accepted after the database revoke until its identity
/// drop arrives. The long TTL cannot expire during this test.
#[tokio::test]
#[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
async fn live_a_drop_refuses_a_revoked_pat_on_the_next_request() {
    use crate::route_authentication::PlatformRouteAuthenticator;
    let live_platform_principal =
        wamn_identity_protocol::PlatformComponent::Executor.principal_id();
    use wamn_platform_identity::{assign_project_role, create_service, issue_pat, revoke_pat};

    wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");

    let _lock = wamn_test_postgres::lock();
    let mut case_server = wamn_test_postgres::start(&[]).unwrap();
    let system = url::Url::parse(
        case_server
            .create_database("fixture_1")
            .await
            .unwrap()
            .url(),
    )
    .unwrap();
    wamn_control_provision::test_database::system(system.as_str()).await;

    let project = url::Url::parse(
        case_server
            .create_database("fixture_extra")
            .await
            .unwrap()
            .url(),
    )
    .unwrap();
    wamn_platform_identity::test_database::project_platform(project.as_str()).await;
    let mut admin = connect(system.as_str()).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "SET ROLE wamn_system; \
         SELECT set_config('app.user_id', '{live_platform_principal}', false); \
         INSERT INTO registry.orgs (id, placement_type, pool_cluster) VALUES ('org', 'pooled', 'test'); \
         INSERT INTO registry.projects (org, id) VALUES ('org', 'project');"
    ))).execute(&mut admin).await.map(drop).unwrap();
    let principal = create_service(
        &mut admin,
        "pat-drop-test",
        "PAT drop test",
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await
    .unwrap();
    assign_project_role(
        &mut admin,
        principal.id(),
        "org",
        "project",
        "admin",
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await
    .unwrap();
    let issued = issue_pat(
        &mut admin,
        principal.id(),
        "drop",
        std::time::Duration::from_secs(7200),
    )
    .await
    .unwrap();
    let mut project_admin = connect(project.as_str()).await;
    sqlx::raw_sql("CREATE SCHEMA wamn_run")
        .execute(&mut project_admin)
        .await
        .map(drop)
        .unwrap();
    let generation = |family| {
        wamn_control_provision::workload_login(
            family,
            wamn_control_provision::WorkloadRoleIdentity::ProjectEnvironment {
                org: "org",
                project: "project",
                environment: "prod",
                database: project.path().trim_start_matches('/'),
            },
        )
        .unwrap()
    };
    let administration = generation(wamn_control_provision::WorkloadRoleFamily::Administration);
    let http = generation(wamn_control_provision::WorkloadRoleFamily::HttpAdmitter);
    for (family, name) in [
        (
            wamn_control_provision::WorkloadRoleFamily::Administration,
            &administration,
        ),
        (
            wamn_control_provision::WorkloadRoleFamily::HttpAdmitter,
            &http,
        ),
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::test_database::login_sql(
                family,
                project.path().trim_start_matches('/'),
                name,
                "pat-drop",
            ))
                .to_string(),
        ))
        .execute(&mut project_admin)
        .await
        .map(drop)
        .unwrap();
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "SET ROLE \"{administration}\"; SET app.user_id = '{live_platform_principal}'; SET app.operation = 'admin:seed-pat-drop-fixture';"
    ))).execute(&mut project_admin).await.map(drop).unwrap();
    sqlx::query("INSERT INTO platform.\"user\" (id, type, email) VALUES ($1::text::uuid, 'service', 'pat-drop@example.test')").bind(&principal.id().to_string().as_str()).execute(&mut project_admin).await.map(|result| result.rows_affected()).unwrap();
    let mut http_url = url::Url::parse(project.as_str()).unwrap();
    http_url.set_username(&http).unwrap();
    http_url.set_password(Some("pat-drop")).unwrap();
    let config = WamnPostgresConfig {
        credentials: None,
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
        row_limit: 100,
    };
    let projects = StaticCredentialProvider::projects_from_json(
        &serde_json::json!({"project": {"credentials": {
            (AuthorityClass::CallableHttp.as_str()): http_url.as_str(),
        }}})
        .to_string(),
        &config,
    )
    .unwrap();
    let mut pg =
        WamnPostgres::with_provider(Arc::new(StaticCredentialProvider::new(projects, None)));
    let ttl = std::time::Duration::from_secs(3600);
    pg = pg.with_auth_cache_for_test(Arc::new(
        wamn_capability_authority::auth_cache::AuthCache::with_ttl(ttl),
    ));
    let pg = Arc::new(pg);
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::grant_identity_reader_surface_sql(),
    ))
    .execute(&mut wamn_postgres::connect(system.as_str()).await.unwrap())
    .await
    .unwrap();
    let identity_reader = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET ROLE wamn_identity_reader")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_lazy_with(
            wamn_postgres::connect_options(system.as_str())
                .await
                .unwrap(),
        );
    let authentication = PlatformRouteAuthenticator::for_test(
        Arc::clone(&pg),
        "project",
        Some(identity_reader),
        None,
    );
    let route = crate::route_authentication::test_util::routing(&serde_json::json!(["pat"]))
        .with_authenticator(Arc::new(authentication));
    let (_nats, client) = wamn_capability_authority::auth_cache::test_util::nats().await;
    let writer = wamn_capability_authority::auth_cache::test_util::watched(
        &client,
        Some("org--dev--a"),
        Arc::clone(pg.auth_cache()),
    )
    .await;
    let authorization = format!("Bearer {}", issued.token());
    let request = || route.authenticate_authorization_for_test("orders", Some(&authorization));
    let cached_at = std::time::Instant::now();
    let caller = request().await.unwrap().unwrap();
    assert_eq!(caller.principal_id(), principal.id().to_string().as_str());
    assert!(pg.auth_cache().pat(issued.token()).is_some());
    let mut drops = wamn_broker::auth_changes::AuthDrops::default();
    revoke_pat(&mut admin, issued.record().prefix(), &mut drops)
        .await
        .unwrap();
    assert!(
        request().await.is_ok(),
        "the cached PAT still answers before the drop"
    );
    writer.publish(drops).await;
    wamn_capability_authority::auth_cache::test_util::until(
        "the identity drop removes the PAT",
        || pg.auth_cache().pat(issued.token()).is_none(),
    )
    .await;
    assert_eq!(
        request().await.unwrap_err(),
        (401, "unauthorized".to_owned())
    );
    assert!(
        cached_at.elapsed() < ttl,
        "only the drop can expire this PAT approval"
    );
}
