use std::sync::Arc;

use serde_json::{Value, json};
use uuid::Uuid;
use wamn_app::app_config::{
    AppComponent, AppConfig, AppConnection, AppOperation, AppOperationType,
};
use wamn_blobstore::file_route::{FileError, FileRefusal};
use wamn_connection::administration;
use wamn_connection::bound_http::{Attempt, BoundHttp, Call, Response as HttpResponse};
use wamn_connection::connection::ConnectionInvocation;
use wamn_connection::connection_facts::ConnectionCache as _;
use wamn_connection::invocation::{ReleaseContext, Scope};
use wamn_connection::project_connections::Administration;
use wamn_identity_protocol::AuthorityClass;
use wamn_package::ConnectionMode;
use wamn_postgres::{ClassCredentials, ProjectConfig, StaticCredentialProvider, WamnPostgres};
use wamn_registry::Triple;

use super::{AppBinding, ConnectionError, ConnectionHttp, InvocationScopes, Request};
use crate::file_connections::RegistryFileConnections;
use crate::host_policy::CONNECTION_HOST_POLICY;

#[derive(Debug)]
struct NoContent;

#[async_trait::async_trait]
impl BoundHttp for NoContent {
    async fn attempt(
        &self,
        _: &Call<'_>,
        _: Attempt<'_>,
    ) -> Result<(HttpResponse, Option<String>), ConnectionError> {
        Ok((
            HttpResponse {
                external_system: None,
                status: 204,
                headers: Vec::new(),
                body: Vec::new(),
            },
            None,
        ))
    }
}

async fn fixture_connection(
    transaction: &mut sqlx::PgConnection,
    name: &str,
    integration: &str,
    parameters: Value,
) -> sqlx::Result<Uuid> {
    let id = sqlx::query_scalar(
        "INSERT INTO platform.connection(name,integration_type,auth_type,target_id,active_generation,service_principal_id)
         VALUES ($1,$2,'none',uuidv7(),1,uuidv7()) RETURNING id",
    )
    .bind(name)
    .bind(integration)
    .fetch_one(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO platform.connection_generation(connection_id,generation,parameters)
         VALUES ($1,1,$2)",
    )
    .bind(id)
    .bind(parameters)
    .execute(&mut *transaction)
    .await?;
    Ok(id)
}

fn binding(
    name: &str,
    interface: &str,
    integration: &str,
    id: Uuid,
) -> anyhow::Result<AppConnection> {
    let name: wamn_package::canonical_name::CanonicalName = name.parse()?;
    Ok(AppConnection {
        selector: name.clone().into(),
        name,
        connection_id: id.to_string(),
        mode: ConnectionMode::Write,
        interface: interface.into(),
        integration_type: integration.parse()?,
        runtime_type: wamn_connection_type::RuntimeType::Host,
        exclusive: false,
        target_type: "cloud".into(),
        integration: None,
    })
}

async fn insert_file(tx: &mut sqlx::PgConnection, store: Uuid) -> sqlx::Result<()> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO platform.file(id,app,connection_id,name,media_type,size,sha256,blob_key)
         VALUES ($1,'probe',$2,'file','text/plain',0,$3,$4)",
    )
    .bind(id)
    .bind(store)
    .bind(wamn_digest::bytes_sha256(b"").to_string())
    .bind(format!("_wamn_files/live/{id}"))
    .execute(tx)
    .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn deactivation_refuses_warm_runtime_bindings() -> anyhow::Result<()> {
    let database = wamn_test_infrastructure::database::project().await?;
    let mut project = database.admin().await?;
    // Deactivation requires no system metadata. This separate session meets the
    // administration interface while keeping every assertion on the project database.
    let mut system = database.admin().await?;
    let mut tx =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut project).await?;
    let http_id = fixture_connection(&mut tx, "web", "http", json!({})).await?;
    let modbus_id = fixture_connection(&mut tx, "device", "modbus", json!({})).await?;
    let store_id = fixture_connection(
        &mut tx,
        "files",
        "gcs",
        json!({"provider":"gcs","container":"wamn-test","prefix":"files"}),
    )
    .await?;
    insert_file(&mut tx, store_id).await?;
    tx.commit().await?;

    let target = Triple::new("acme".parse()?, "dev".parse()?, "billing".parse()?);
    let login = database.login_url(
        wamn_identity_protocol::workload_role::WorkloadRoleFamily::Platform.login_name(),
    )?;
    let pools = Arc::new(WamnPostgres::with_provider(Arc::new(
        StaticCredentialProvider::new(
            [(
                target.project.clone(),
                ProjectConfig {
                    credentials: ClassCredentials::default()
                        .with_class(AuthorityClass::Platform, login),
                    guest_pool_max_size: 1,
                    platform_pool_max_size: 2,
                    wait_timeout_ms: 2000,
                    statement_timeout_ms: 5000,
                },
            )]
            .into(),
            None,
        ),
    )));
    let operation: wamn_package::canonical_name::CanonicalName = "probe".parse()?;
    let app = Arc::new(AppConfig {
        org: target.org.clone(),
        project: target.project.clone(),
        env: target.env.clone(),
        app: "probe".parse()?,
        grant_cache_seconds: 3600,
        components: vec![AppComponent {
            name: "probe".into(),
            package: "probe".parse()?,
            digest: wamn_digest::bytes_sha256(b"probe"),
            connections: vec![
                binding("web", "wamn:connection/http", "http", http_id)?,
                binding("files", "wasmcloud:blobstore", "gcs", store_id)?,
            ],
            schema: None,
            operations: [(
                operation.clone(),
                AppOperation {
                    component: "probe".into(),
                    public: true,
                    host_transaction: false,
                    type_: Some(AppOperationType::Read),
                    idempotency_key: None,
                    signed_result: None,
                },
            )]
            .into(),
            data_change: None,
        }],
    });
    let authorization = Arc::new(wamn_authentication::authorization::Authorization::new(
        pools,
    ));
    let facts = Arc::clone(&authorization).facts(Arc::clone(&app))?;
    let scopes = Arc::new(InvocationScopes::default());
    let context = Arc::new(ReleaseContext::new(Arc::clone(&app), Arc::clone(&facts))?);
    scopes.update::<Scope, _>("scope", |scope| {
        scope.context = Some(context);
        scope.invocation = Some(ConnectionInvocation::for_operation(&app, &operation).unwrap());
    });
    let mut http = ConnectionHttp::new(scopes, Arc::new(NoContent), &CONNECTION_HOST_POLICY);
    http.component = Some("probe".into());
    http.apps.write().unwrap().insert(
        "probe".into(),
        AppBinding {
            workload: "fixture".into(),
            app: Arc::clone(&app),
        },
    );
    let request = || Request {
        requirement: "web".into(),
        method: "GET".into(),
        path_and_query: "/".into(),
        headers: Vec::new(),
        body: None,
        idempotency_key: None,
    };
    let blobstore = crate::blobstore::WamnBlobstore::new(Arc::clone(&http.scopes), Arc::default());
    let file_connections = RegistryFileConnections {
        release: Arc::clone(&app),
        facts: Arc::clone(&facts),
    };
    let file_binding = "files".parse()?;
    assert_eq!(http.send("scope", request()).await?.status, 204);
    assert!(
        crate::integrations::begin_fixture(&facts, modbus_id)
            .await?
            .is_ok()
    );
    let container = blobstore.container_for("scope", &file_binding).await?;
    file_connections
        .lookup(&app.app, Some(&file_binding), None)
        .await?;

    let mut tx =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut project).await?;
    for name in ["web", "device", "files"] {
        let mut administration = Administration::new(&mut tx, &mut system, None);
        administration::configure(
            &mut administration,
            &target.org,
            "connection.deactivate",
            &json!({"project":target.project,"env":target.env,"name":name}),
        )
        .await?;
    }
    tx.commit().await?;

    assert!(matches!(
        http.send("scope", request()).await,
        Err(ConnectionError::Unbound)
    ));
    assert!(
        crate::integrations::begin_fixture(&facts, modbus_id)
            .await?
            .is_err()
    );
    assert!(
        blobstore
            .container_for("scope", &file_binding)
            .await
            .is_err()
    );
    assert!(matches!(
        container.put("item", Vec::new()).await,
        Err(wamn_blobstore::store::StoreError::Refused { .. })
    ));
    assert!(matches!(
        file_connections
            .lookup(&app.app, Some(&file_binding), None)
            .await,
        Err(FileError {
            refusal: Some(FileRefusal::BindingRefused),
            ..
        })
    ));
    let mut tx =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut project).await?;
    let error = insert_file(&mut tx, store_id).await.unwrap_err();
    assert!(
        error
            .as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_check_violation)
    );
    tx.rollback().await?;

    Ok(())
}
