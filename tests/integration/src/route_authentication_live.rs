//! Production route authentication and exact-operation authorization on one
//! fresh disposable PostgreSQL 18 server.

use std::collections::{BTreeSet, HashMap};
use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::{Method, Request, StatusCode};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{
    InMemorySpanExporter, InMemorySpanExporterBuilder, SdkTracerProvider, SpanData,
};
use reqwest::Url;
use serde_json::Value;
use tokio_postgres::{Client, NoTls};
use tracing_subscriber::layer::SubscriberExt as _;
use wamn_catalog::{PackageCoordinate, SERVING_MANIFEST_FORMAT_VERSION};
use wamn_control_provision::{
    CONTROL_PORTABLE_STORE_SQL, CredentialGeneration, SYSTEM_SCHEMA_SQL, SystemReader,
    WorkloadRoleFamily, parse_system_reader_url, sql as provision_sql,
};
use wamn_ctl::apply_package::{self, ApplyPackageArgs};
use wamn_ctl::author_wiring::{self, AuthorWiringArgs};
use wamn_ctl::provision_org::{self, ProvisionOrgArgs, TemplateArg};
use wamn_ctl::provision_project_env::{
    self, ProvisionProjectEnvArgs, WorkloadActionVerb, WorkloadGenerationAction,
    WorkloadGenerationArgs,
};
use wamn_ctl::publish_release::{self, PublishReleaseArgs, ReleaseWiringTarget};
use wamn_ctl::push_component::{self, PushComponentArgs};
use wamn_ctl::push_release_manifest::{self, PushReleaseManifestArgs};
use wamn_ctl::reconcile_package_data_access::{self, ReconcilePackageDataAccessArgs};
use wamn_ctl::reconcile_run_plane::{self, ReconcileRunPlaneArgs};
use wamn_execution_host::{
    ROUTER_DELIVERY_ID, RouterDeliveryBridge, RouterDriver, RouterDriverConfig,
    WiringCacheCapacity, authorize_attachment_for_test,
};
use wamn_platform_identity::{
    PrincipalKind, assign_project_role, create_service, issue_pat, resolve_subject, revoke_pat,
    route_caller_subject,
};
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_runtime::engine::build_engine;
use wamn_runtime::plugins::WamnJetstream;
use wamn_runtime::plugins::flow_http_routing::{
    FLOW_HTTP_ROUTING_ID, FlowHttpRouting, RouteAuthentication, RouteInFlightLimit,
};
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_jetstream::WamnJetstreamConfig;
use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, CredentialProvider, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};
use wamn_runtime::release_manifest::ReleaseManifestWeld;
use wamn_runtime::release_manifest_source::ReleaseManifestSource;
use wamn_scenario_worker::management::{self, ManagementServeArgs};
use wash_runtime::engine::InstancePolicy;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::engine::workload::{WorkloadComponent, WorkloadItem};
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::types::LocalResources;
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component, Linker};
use wasmtime_wasi_http::p2::WasiHttpView as _;
use wasmtime_wasi_http::p2::bindings::Proxy;
use wasmtime_wasi_http::p2::bindings::http::types::{ErrorCode, Scheme};

const URL_ENV: &str = "WAMN_ROUTE_AUTH_PG18_URL";
const JOURNEY_URL_ENV: &str = "WAMN_RECEIVING_ROUTE_PG18_URL";
const ORG: &str = "acme";
const PROJECT: &str = "receiving";
const OTHER_PROJECT: &str = "other";
const ENVIRONMENT: &str = "dev";
const OTHER_ENVIRONMENT: &str = "prod";
const TENANT: &str = "receiving-route-auth";
const ROUTE_CALLER_ROLE: &str = "route-caller";
const ATTACHMENT_ID: &str = "receiving-purchase-order-get";
const OPERATION: &str = "wamn_receiving@1.0.0::purchase_order.get";
const RESIDUE: &str = "wamn_receiving@1.0.0::obsolete.operation";
const PACKAGE_ID: &str = "wamn_receiving";
const PACKAGE_VERSION: &str = "1.0.0";
const RELEASE_ID: u32 = 1;
const RAW_BODY_LIMIT: usize = 1024 * 1024;
const REGISTRY_IO_TIMEOUT: Duration = Duration::from_secs(30);
const COMPONENTS: [(&str, &str); 6] = [
    ("purchase_order_get", "purchase_order.get"),
    ("purchase_order_query", "purchase_order.query"),
    ("purchase_order_update", "purchase_order.update"),
    ("receipt_get", "receipt.get"),
    ("receipt_query", "receipt.query"),
    ("receiving_record_receipt", "receiving.record_receipt"),
];

#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    Authentication(u16, String),
    Permission(Box<str>),
}

struct ProvisionedRoute {
    database_url: String,
    token: String,
    token_prefix: String,
    principal_subject: String,
    management_token: Option<String>,
    management_principal_subject: Option<String>,
}

struct ScratchRoot(PathBuf);

impl ScratchRoot {
    fn create() -> anyhow::Result<Self> {
        let path = scratch_root();
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).context("create route-auth proof directory")?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn connect(url: &str) -> anyhow::Result<(Arc<Client>, tokio::task::JoinHandle<()>)> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect to disposable PostgreSQL")?;
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((Arc::new(client), task))
}

fn database_url(admin_url: &str, database: &str) -> anyhow::Result<String> {
    let mut url = Url::parse(admin_url).context("parse disposable PostgreSQL URL")?;
    url.set_path(&format!("/{database}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

fn scratch_root() -> PathBuf {
    std::env::temp_dir().join(format!("route-authentication-live-{}", std::process::id()))
}

fn provisioning_args(
    system_url: &str,
    root: &Path,
    route_secret: &Path,
    management_secret: Option<&Path>,
) -> ProvisionProjectEnvArgs {
    ProvisionProjectEnvArgs {
        org: Some(ORG.to_owned()),
        project: Some(PROJECT.to_owned()),
        env: Some(ENVIRONMENT.to_owned()),
        tenant: Some(TENANT.to_owned()),
        system_database_url: Some(system_url.to_owned()),
        cluster: Some("route-auth-pg18".to_owned()),
        connection_limit: None,
        app_password: Some("unused-legacy-secret".to_owned()),
        app_host: Some("route-auth-pg18.invalid".to_owned()),
        app_port: 5432,
        namespace: "wamn-system".to_owned(),
        secret_namespace: None,
        target_admin_database_url: None,
        workload: WorkloadGenerationArgs::default(),
        emit_database: Some(root.join("database.json")),
        emit_role_sql: Some(root.join("roles.sql")),
        emit_privilege_sql: Some(root.join("privileges.sql")),
        emit_secret: Some(root.join("database-secret.json")),
        emit_management_author_pat_secret: management_secret.map(Path::to_path_buf),
        emit_route_caller_pat_secret: Some(route_secret.to_path_buf()),
        revoke_pat_prefix: None,
    }
}

fn generation_args(
    family: WorkloadRoleFamily,
    system_url: &str,
    target_admin_url: Option<&str>,
    secret: &Path,
) -> ProvisionProjectEnvArgs {
    ProvisionProjectEnvArgs {
        org: Some(ORG.to_owned()),
        project: Some(PROJECT.to_owned()),
        env: Some(ENVIRONMENT.to_owned()),
        tenant: Some(TENANT.to_owned()),
        system_database_url: Some(system_url.to_owned()),
        cluster: None,
        connection_limit: None,
        app_password: None,
        app_host: None,
        app_port: 5432,
        namespace: "wamn-system".to_owned(),
        secret_namespace: None,
        target_admin_database_url: target_admin_url.map(str::to_owned),
        workload: WorkloadGenerationArgs {
            action: Some(WorkloadGenerationAction {
                family,
                verb: WorkloadActionVerb::Prepare,
                generation: CredentialGeneration::A,
            }),
            secret: Some((family, secret.to_path_buf())),
        },
        emit_database: None,
        emit_role_sql: None,
        emit_privilege_sql: None,
        emit_secret: None,
        emit_management_author_pat_secret: None,
        emit_route_caller_pat_secret: None,
        revoke_pat_prefix: None,
    }
}

fn read_json(path: &Path) -> anyhow::Result<Value> {
    serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))
}

fn secret_value(path: &Path, key: &str) -> anyhow::Result<String> {
    read_json(path)?["stringData"][key]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("{} carries stringData.{key}", path.display()))
}

fn secret_annotation(path: &Path, key: &str) -> anyhow::Result<String> {
    read_json(path)?["metadata"]["annotations"][key]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("{} carries annotation {key}", path.display()))
}

async fn reset_control_store(admin: &Client) -> anyhow::Result<()> {
    let stale_databases = admin
        .query(
            "SELECT datname::text FROM pg_database \
             WHERE datname LIKE 'wamn-db-acme--%--%--%' ORDER BY datname",
            &[],
        )
        .await
        .context("list stale route-auth databases")?;
    for row in stale_databases {
        let database: String = row.get(0);
        admin
            .batch_execute(&provision_sql::drop_database_named_sql(&database))
            .await
            .with_context(|| format!("drop stale database {database}"))?;
    }
    admin
        .batch_execute(
            "DROP SCHEMA IF EXISTS identity CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
                 CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOREPLICATION NOBYPASSRLS; \
               END IF; \
             END $$; \
             DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', \
                                        current_database()); END $$;",
        )
        .await
        .context("prepare the production control owner")?;
    admin
        .batch_execute(&provision_sql::ensure_control_author_acl_role_sql())
        .await
        .context("ensure the portable store's control-author ACL role")?;
    admin
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("assume the production control owner")?;
    admin
        .batch_execute(SYSTEM_SCHEMA_SQL)
        .await
        .context("install deploy/sql/system-schema.sql")?;
    admin
        .batch_execute(CONTROL_PORTABLE_STORE_SQL)
        .await
        .context("install the control portable store")?;
    admin
        .batch_execute("RESET ROLE")
        .await
        .context("release the production control owner before cluster ACL convergence")?;
    admin
        .batch_execute(provision_sql::revoke_public_connect_floor_sql())
        .await
        .context("converge the cluster PUBLIC CONNECT floor")?;
    admin
        .batch_execute(
            "DO $$ BEGIN EXECUTE format(\
               'REVOKE TEMPORARY ON DATABASE %I FROM PUBLIC', current_database()); END $$;",
        )
        .await
        .context("converge the control database PUBLIC TEMPORARY floor")?;
    Ok(())
}

async fn reset_and_install_control(admin: &Client) -> anyhow::Result<()> {
    reset_control_store(admin).await?;
    admin
        .batch_execute(
            r#"RESET ROLE;
               SET ROLE wamn_system;
               INSERT INTO registry.orgs (id, placement_kind, pool_cluster)
               VALUES ('acme', 'pooled', 'route-auth-pg18');
               INSERT INTO registry.env_policies
                 (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
               VALUES
                 ('acme', 'dev', '"own"'::jsonb, 1, 1, '1Gi', '1', '1Gi', 'postgres:18'),
                 ('acme', 'prod', '"own"'::jsonb, 2, 1, '1Gi', '1', '1Gi', 'postgres:18');
               RESET ROLE;"#,
        )
        .await
        .context("seed the auth-only test's declared environment policies")?;
    Ok(())
}

async fn provision_route(
    system_url: &str,
    admin: &Client,
    root: &Path,
    management_secret: Option<&Path>,
) -> anyhow::Result<ProvisionedRoute> {
    let route_secret = root.join("route-caller-pat.json");
    provision_project_env::run(provisioning_args(
        system_url,
        root,
        &route_secret,
        management_secret,
    ))
    .await
    .context("run production project-environment and route-PAT provisioning")?;

    let database = read_json(&root.join("database.json"))?["spec"]["name"]
        .as_str()
        .context("Database CR carries spec.name")?
        .to_owned();
    admin
        .batch_execute(
            &std::fs::read_to_string(root.join("roles.sql")).context("read emitted role SQL")?,
        )
        .await
        .context("apply emitted role SQL")?;
    admin
        .batch_execute(&wamn_schema_control::ensure_scenario_author_role_sql())
        .await
        .context("ensure the catalog author role")?;
    admin
        .batch_execute(&provision_sql::create_database_named_sql(&database))
        .await
        .context("stand in for the emitted Database CR")?;
    admin
        .batch_execute(
            &std::fs::read_to_string(root.join("privileges.sql"))
                .context("read emitted privilege SQL")?,
        )
        .await
        .context("apply emitted privilege SQL")?;

    Ok(ProvisionedRoute {
        database_url: database_url(system_url, &database)?,
        token: secret_value(&route_secret, "token")?,
        token_prefix: secret_annotation(&route_secret, "wamn.io/pat-prefix")?,
        principal_subject: secret_annotation(&route_secret, "wamn.io/principal-subject")?,
        management_token: management_secret
            .map(|secret| secret_value(secret, "token"))
            .transpose()?,
        management_principal_subject: management_secret
            .map(|secret| secret_annotation(secret, "wamn.io/principal-subject"))
            .transpose()?,
    })
}

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/receiving")
}

async fn permission_write_identity(project: &Client) -> anyhow::Result<Vec<String>> {
    Ok(project
        .query(
            "SELECT permission || ':' || xmin::text FROM app_system.permissions \
             WHERE tenant_id = $1 AND role_name = $2 ORDER BY permission COLLATE \"C\"",
            &[&TENANT, &ROUTE_CALLER_ROLE],
        )
        .await
        .context("read permission write identities")?
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect())
}

async fn install_project_and_reconcile(project: &Client, project_url: &str) -> anyhow::Result<()> {
    project
        .batch_execute(include_str!("../../../deploy/sql/catalog-schema.sql"))
        .await
        .context("install catalog schema")?;
    project
        .batch_execute(include_str!("../../../deploy/sql/app-schema.sql"))
        .await
        .context("install application authorization schema")?;
    project
        .batch_execute("CREATE SCHEMA wamn_run AUTHORIZATION postgres")
        .await
        .context("create the empty run-plane revoke scope")?;
    project
        .execute(
            "INSERT INTO app_system.roles (tenant_id, name, is_system) \
             VALUES ($1, $2, false)",
            &[&TENANT, &ROUTE_CALLER_ROLE],
        )
        .await
        .context("seed the route-caller role")?;
    project
        .execute(
            "INSERT INTO app_system.permissions (tenant_id, role_name, permission) \
             VALUES ($1, $2, $3)",
            &[&TENANT, &ROUTE_CALLER_ROLE, &RESIDUE],
        )
        .await
        .context("seed package-coordinate residue")?;

    let args = || ApplyPackageArgs {
        package: package_root(),
        database_url: project_url.to_owned(),
        tenant: TENANT.to_owned(),
    };
    apply_package::run(args())
        .await
        .context("apply the Receiving package")?;
    let expected = wamn_control_provision::operation_grants::operation_grant_tokens(
        include_bytes!("../../../packages/receiving/wamn.json"),
    )
    .context("derive the strict manifest's operation tokens")?;
    let observed = project
        .query(
            "SELECT permission::text FROM app_system.permissions \
             WHERE tenant_id = $1 AND role_name = $2 ORDER BY permission COLLATE \"C\"",
            &[&TENANT, &ROUTE_CALLER_ROLE],
        )
        .await
        .context("read reconciled operation grants")?
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        observed, expected,
        "the real reconciler must author the manifest set"
    );
    assert_eq!(
        observed.len(),
        6,
        "Receiving declares exactly six operations"
    );
    assert!(
        !observed.contains(RESIDUE),
        "coordinate residue survived reconcile"
    );

    let before = permission_write_identity(project).await?;
    apply_package::run(args())
        .await
        .context("replay the converged Receiving package")?;
    assert_eq!(permission_write_identity(project).await?, before);
    Ok(())
}

fn serving_weld() -> anyhow::Result<Arc<ReleaseManifestWeld>> {
    let definition = serde_json::json!({
        "id": ATTACHMENT_ID,
        "kind": "http",
        "route": {
            "host": "receiving.example.test",
            "path": "/purchase-orders/get",
            "method": "POST"
        }
    });
    let definition_hash = wamn_execution_contract::canonical_json_sha256(&definition);
    let manifest = serde_json::json!({
        "format-version": SERVING_MANIFEST_FORMAT_VERSION,
        "release": {
            "tenant-id": TENANT,
            "effective-release-id": 1,
            "environment": ENVIRONMENT,
            "packages": [{"package-id": "wamn_receiving", "package-version": "1.0.0"}]
        },
        "components": [{
            "package-id": "wamn_receiving",
            "component": "purchase-order-get",
            "interface-version": "0.1.0",
            "digest": format!("sha256:{}", "a".repeat(64)),
            "registered-operation": OPERATION
        }],
        "wirings": [{
            "package-id": "wamn_receiving",
            "wiring-id": "purchase-order-get",
            "wiring-version": 1,
            "graph-hash": format!("sha256:{}", "b".repeat(64))
        }],
        "attachments": {
            (ATTACHMENT_ID): {
                "kind": "http",
                "package-id": "wamn_receiving",
                "wiring-id": "purchase-order-get",
                "wiring-version": 1,
                "definition-hash": definition_hash,
                "definition": definition,
                "auth-policy": {"mode": "pat"},
                "registered-operation": OPERATION
            }
        },
        "registrations": {}
    });
    let bytes = wamn_execution_contract::canonical_json_bytes(&manifest);
    Ok(Arc::new(ReleaseManifestWeld::load_canonical_bytes(
        &bytes,
        "route-authentication-live fixture",
    )?))
}

fn project_postgres(class: AuthorityClass, url: &str) -> anyhow::Result<Arc<WamnPostgres>> {
    let base = WamnPostgresConfig {
        credentials: None,
        guest_pool_max_size: 1,
        platform_pool_max_size: 2,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
        row_limit: 100,
    };
    let configuration = serde_json::json!({
        PROJECT: {"credentials": {(class.as_str()): url}}
    });
    let projects = StaticCredentialProvider::projects_from_json(&configuration.to_string(), &base)?;
    let provider: Arc<dyn CredentialProvider> =
        Arc::new(StaticCredentialProvider::new(projects, None));
    Ok(Arc::new(WamnPostgres::with_provider(provider)))
}

fn routing(
    identity_reader: Arc<Client>,
    postgres: Arc<WamnPostgres>,
    weld: Arc<ReleaseManifestWeld>,
) -> anyhow::Result<FlowHttpRouting> {
    Ok(
        FlowHttpRouting::new(Some(weld), RouteInFlightLimit::default()).with_authentication(
            Arc::new(RouteAuthentication::new(
                identity_reader,
                postgres,
                ORG,
                PROJECT,
                route_caller_subject(ORG, PROJECT, ENVIRONMENT)?,
            )),
        ),
    )
}

async fn invoke(
    routing: &FlowHttpRouting,
    weld: &ReleaseManifestWeld,
    authorization: Option<&str>,
    router_admissions: &mut usize,
) -> Result<(), Refusal> {
    let caller = routing
        .authenticate_authorization_for_test(ATTACHMENT_ID, authorization)
        .await
        .map_err(|(status, code)| Refusal::Authentication(status, code))?;
    authorize_attachment_for_test(weld, ATTACHMENT_ID, caller.as_ref())
        .map_err(Refusal::Permission)?;
    *router_admissions += 1;
    Ok(())
}

async fn issue_scoped_token(
    admin: &Client,
    project: &str,
    environment: &str,
) -> anyhow::Result<String> {
    let subject = route_caller_subject(ORG, project, environment)?;
    let principal = create_service(
        admin,
        &subject,
        &format!("route caller {project}/{environment}"),
    )
    .await
    .context("create wrong-scope route caller")?;
    assign_project_role(admin, principal.id(), ORG, project, ROUTE_CALLER_ROLE)
        .await
        .context("assign wrong-scope route-caller role")?;
    Ok(issue_pat(
        admin,
        principal.id(),
        "route-caller",
        Duration::from_secs(3600),
    )
    .await
    .context("issue wrong-scope route PAT")?
    .token()
    .to_owned())
}

async fn issue_pat_for_subject(
    admin: &Client,
    subject: &str,
    label: &str,
) -> anyhow::Result<(String, String)> {
    let principal = resolve_subject(admin, PrincipalKind::Service, subject)
        .await
        .context("resolve route-caller principal")?
        .context("route-caller principal is absent")?;
    let issued = issue_pat(admin, principal.id(), label, Duration::from_secs(3600))
        .await
        .with_context(|| format!("issue {label} PAT"))?;
    Ok((
        issued.token().to_owned(),
        issued.record().prefix().to_owned(),
    ))
}

fn flip_last_hex_digit(token: &str) -> String {
    let (head, last) = token.split_at(token.len() - 1);
    let replacement = if last == "a" { 'b' } else { 'a' };
    format!("{head}{replacement}")
}

#[tokio::test]
#[ignore = "requires a fresh disposable PG18 named by WAMN_ROUTE_AUTH_PG18_URL"]
async fn production_route_caller_authentication_and_operation_authorization() {
    let admin_url = std::env::var(URL_ENV)
        .expect("WAMN_ROUTE_AUTH_PG18_URL must name a fresh disposable PostgreSQL 18 server");
    let scratch = ScratchRoot::create().expect("create route-auth proof directory");
    let root = scratch.path();

    let (admin, admin_task) = connect(&admin_url).await.expect("connect admin");
    let version: i32 = admin
        .query_one("SHOW server_version_num", &[])
        .await
        .expect("read PostgreSQL version")
        .get::<_, String>(0)
        .parse()
        .expect("parse PostgreSQL version");
    assert!(
        version >= 180_000,
        "the gate requires PostgreSQL 18 or newer"
    );
    reset_and_install_control(&admin)
        .await
        .expect("install the control plane");
    let route = provision_route(&admin_url, &admin, &root, None)
        .await
        .expect("mint the production route caller");
    assert_eq!(
        route.principal_subject,
        route_caller_subject(ORG, PROJECT, ENVIRONMENT).expect("derive expected route subject")
    );

    let (project, project_task) = connect(&route.database_url)
        .await
        .expect("connect project database");
    install_project_and_reconcile(&project, &route.database_url)
        .await
        .expect("install and reconcile Receiving");

    let identity_secret = root.join("identity-reader.json");
    provision_project_env::run(generation_args(
        WorkloadRoleFamily::IdentityReader,
        &admin_url,
        None,
        &identity_secret,
    ))
    .await
    .expect("prepare the production identity-reader generation");
    let http_secret = root.join("http-admitter.json");
    provision_project_env::run(generation_args(
        WorkloadRoleFamily::HttpAdmitter,
        &admin_url,
        Some(&route.database_url),
        &http_secret,
    ))
    .await
    .expect("prepare the production callable-HTTP generation");

    let identity_url = secret_value(&identity_secret, "url").expect("read identity-reader URL");
    parse_system_reader_url(
        SystemReader::Identity,
        &identity_url,
        ORG,
        PROJECT,
        ENVIRONMENT,
    )
    .expect("the identity-reader Secret passes its consumer's exact scope gate");
    let (identity_reader, identity_task) = connect(&identity_url)
        .await
        .expect("connect exact identity-reader generation");
    let http_url = secret_value(&http_secret, "url").expect("read callable-HTTP URL");
    let weld = serving_weld().expect("load the canonical serving weld");
    let route_auth = routing(
        Arc::clone(&identity_reader),
        project_postgres(AuthorityClass::CallableHttp, &http_url)
            .expect("build project-specific callable-HTTP provider"),
        Arc::clone(&weld),
    )
    .expect("build route authentication");

    let mut router_admissions = 0;
    let valid = format!("Bearer {}", route.token);
    invoke(&route_auth, &weld, Some(&valid), &mut router_admissions)
        .await
        .expect("production-minted caller reaches the production router authorization boundary");
    assert_eq!(router_admissions, 1);

    let forged = format!("Bearer {}", flip_last_hex_digit(&route.token));
    let expired = issue_pat_for_subject(&admin, &route.principal_subject, "expired")
        .await
        .expect("mint expiring PAT");
    admin
        .execute(
            "UPDATE identity.pats SET created_at = now() - interval '2 hours', \
             expires_at = now() - interval '1 hour' WHERE token_prefix = $1",
            &[&expired.1],
        )
        .await
        .expect("expire PAT in the server clock");
    let revoked = issue_pat_for_subject(&admin, &route.principal_subject, "revoked")
        .await
        .expect("mint revocable PAT");
    revoke_pat(admin.as_ref(), &revoked.1)
        .await
        .expect("revoke PAT");
    admin
        .execute(
            "INSERT INTO registry.projects (org, id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            &[&ORG, &OTHER_PROJECT],
        )
        .await
        .expect("seed wrong-project role scope");
    let wrong_project = issue_scoped_token(&admin, OTHER_PROJECT, ENVIRONMENT)
        .await
        .expect("mint wrong-project PAT");
    let wrong_environment = issue_scoped_token(&admin, PROJECT, OTHER_ENVIRONMENT)
        .await
        .expect("mint wrong-environment PAT");
    let missing_role = issue_pat_for_subject(&admin, &route.principal_subject, "missing-role")
        .await
        .expect("mint missing-role PAT");
    admin
        .execute(
            "DELETE FROM identity.project_roles WHERE principal_id = \
               (SELECT id FROM identity.principals WHERE kind = 'service' AND subject = $1) \
               AND org = $2 AND project = $3 AND role = $4",
            &[&route.principal_subject, &ORG, &PROJECT, &ROUTE_CALLER_ROLE],
        )
        .await
        .expect("remove the route-caller role");

    let unauthorized = Refusal::Authentication(401, "unauthorized".to_owned());
    for (label, authorization) in [
        ("absent", None),
        ("malformed", Some("Bearer malformed".to_owned())),
        ("forged", Some(forged)),
        ("expired", Some(format!("Bearer {}", expired.0))),
        ("revoked", Some(format!("Bearer {}", revoked.0))),
        ("wrong-project", Some(format!("Bearer {wrong_project}"))),
        (
            "wrong-environment",
            Some(format!("Bearer {wrong_environment}")),
        ),
        ("missing-role", Some(format!("Bearer {}", missing_role.0))),
    ] {
        assert_eq!(
            invoke(
                &route_auth,
                &weld,
                authorization.as_deref(),
                &mut router_admissions,
            )
            .await
            .expect_err(label),
            unauthorized,
            "{label} disclosed a credential-state distinction"
        );
        assert_eq!(router_admissions, 1, "{label} reached router admission");
    }

    let principal = resolve_subject(
        admin.as_ref(),
        PrincipalKind::Service,
        &route.principal_subject,
    )
    .await
    .expect("resolve route caller")
    .expect("route caller remains stored");
    assign_project_role(
        admin.as_ref(),
        principal.id(),
        ORG,
        PROJECT,
        ROUTE_CALLER_ROLE,
    )
    .await
    .expect("restore route-caller role");
    project
        .execute(
            "DELETE FROM app_system.permissions \
             WHERE tenant_id = $1 AND role_name = $2 AND permission = $3",
            &[&TENANT, &ROUTE_CALLER_ROLE, &OPERATION],
        )
        .await
        .expect("remove the exact operation grant");
    assert_eq!(
        invoke(&route_auth, &weld, Some(&valid), &mut router_admissions,)
            .await
            .expect_err("missing permission must refuse"),
        Refusal::Permission(OPERATION.into())
    );
    assert_eq!(
        router_admissions, 1,
        "missing permission reached router admission"
    );

    let permission_backend_unavailable = routing(
        Arc::clone(&identity_reader),
        project_postgres(
            AuthorityClass::ExecutorPlatform,
            "postgresql://unused.invalid/unused",
        )
        .expect("build provider missing the callable-HTTP credential"),
        Arc::clone(&weld),
    )
    .expect("build permission-unavailable routing");
    assert_eq!(
        invoke(
            &permission_backend_unavailable,
            &weld,
            Some(&valid),
            &mut router_admissions,
        )
        .await
        .expect_err("missing permission authority must be availability"),
        Refusal::Authentication(503, "authentication-unavailable".to_owned())
    );
    assert_eq!(
        router_admissions, 1,
        "permission outage reached router admission"
    );

    identity_task.abort();
    let _ = identity_task.await;
    assert_eq!(
        invoke(&route_auth, &weld, Some(&valid), &mut router_admissions,)
            .await
            .expect_err("identity outage must be availability"),
        Refusal::Authentication(503, "authentication-unavailable".to_owned())
    );
    assert_eq!(
        router_admissions, 1,
        "identity outage reached router admission"
    );

    assert_eq!(route.token_prefix.len(), 16);
    drop(project);
    project_task.abort();
    admin_task.abort();
}

struct JourneyInputs {
    component_directory: PathBuf,
    flow_http_wasm: PathBuf,
    component_artifact_base: String,
    release_artifact_base: String,
    route_host: String,
    registry_auth_file: PathBuf,
    host_secret_directory: PathBuf,
    host_secret_namespace: String,
    route_caller_secret_output: PathBuf,
}

impl JourneyInputs {
    fn required() -> anyhow::Result<Self> {
        Ok(Self {
            component_directory: required_journey_path("WAMN_RECEIVING_ROUTE_COMPONENT_DIRECTORY")?,
            flow_http_wasm: required_journey_path("WAMN_RECEIVING_ROUTE_FLOW_HTTP_WASM")?,
            component_artifact_base: required_journey(
                "WAMN_RECEIVING_ROUTE_COMPONENT_ARTIFACT_BASE",
            )?,
            release_artifact_base: required_journey("WAMN_RECEIVING_ROUTE_RELEASE_ARTIFACT_BASE")?,
            route_host: required_journey("WAMN_RECEIVING_ROUTE_HOST")?,
            registry_auth_file: required_journey_path("WAMN_RECEIVING_ROUTE_REGISTRY_AUTH_FILE")?,
            host_secret_directory: required_journey_path(
                "WAMN_RECEIVING_ROUTE_SECRET_OUTPUT_DIRECTORY",
            )?,
            host_secret_namespace: required_journey("WAMN_RECEIVING_ROUTE_SECRET_NAMESPACE")?,
            route_caller_secret_output: required_journey_path(
                "WAMN_RECEIVING_ROUTE_CALLER_SECRET_OUTPUT",
            )?,
        })
    }
}

struct JourneyCredentials {
    guest_sql: String,
    executor_platform: String,
    http_admitter: String,
    identity_reader: String,
    control_author: String,
    management_admitter: String,
}

struct TraceHarness {
    exporter: InMemorySpanExporter,
    provider: SdkTracerProvider,
    _guard: tracing::subscriber::DefaultGuard,
}

impl TraceHarness {
    fn install() -> Self {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
        let exporter = InMemorySpanExporterBuilder::new().build();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry().with(
            tracing_opentelemetry::layer().with_tracer(provider.tracer("receiving-route-live")),
        );
        let guard = tracing::subscriber::set_default(subscriber);
        Self {
            exporter,
            provider,
            _guard: guard,
        }
    }

    fn spans(&self) -> Vec<SpanData> {
        self.provider
            .force_flush()
            .expect("Receiving route spans must flush");
        self.exporter
            .get_finished_spans()
            .expect("Receiving route span exporter must remain readable")
    }
}

fn required_journey(key: &str) -> anyhow::Result<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("set {key} for the disposable Receiving route journey"))
}

fn required_journey_path(key: &str) -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(required_journey(key)?))
}

fn publication_root() -> PathBuf {
    package_root().join("publication")
}

fn journey_trace(index: u64) -> (String, String) {
    let trace_id = format!("{index:032x}");
    let span_id = format!("{index:016x}");
    (trace_id.clone(), format!("00-{trace_id}-{span_id}-01"))
}

fn span_attribute(span: &SpanData, key: &str) -> Option<String> {
    span.attributes
        .iter()
        .find(|attribute| attribute.key.as_str() == key)
        .map(|attribute| attribute.value.to_string())
}

fn span_descends_from(spans: &[SpanData], span: &SpanData, ancestor: &SpanData) -> bool {
    let trace_id = span.span_context.trace_id();
    let ancestor_id = ancestor.span_context.span_id();
    let mut parent_id = span.parent_span_id;
    for _ in 0..=spans.len() {
        if parent_id == ancestor_id {
            return true;
        }
        let Some(parent) = spans.iter().find(|candidate| {
            candidate.span_context.trace_id() == trace_id
                && candidate.span_context.span_id() == parent_id
        }) else {
            return false;
        };
        parent_id = parent.parent_span_id;
    }
    false
}

fn assert_route_trace(spans: &[SpanData], trace_id: &str, wiring_id: &str, component_digest: &str) {
    let components = spans
        .iter()
        .filter(|span| {
            span.name == "wamn.component.invoke"
                && span.span_context.trace_id().to_string() == trace_id
        })
        .collect::<Vec<_>>();
    assert_eq!(
        components.len(),
        1,
        "trace {trace_id} must contain one released component invocation"
    );
    let component = components[0];
    assert_eq!(
        span_attribute(component, "wamn.wiring_id").as_deref(),
        Some(wiring_id),
        "trace {trace_id} reached a different released wiring"
    );
    assert_eq!(
        span_attribute(component, "wamn.project").as_deref(),
        Some(PROJECT),
        "trace {trace_id} escaped the Receiving project"
    );
    assert_eq!(
        span_attribute(component, "wamn.component_digest").as_deref(),
        Some(component_digest),
        "trace {trace_id} invoked a different released component"
    );
    assert_eq!(
        span_attribute(component, "wamn.node_id").as_deref(),
        Some("operation"),
        "trace {trace_id} invoked a different wiring node"
    );
    let postgres = spans
        .iter()
        .filter(|span| {
            span.name == "wamn.postgres" && span.span_context.trace_id().to_string() == trace_id
        })
        .collect::<Vec<_>>();
    assert!(
        !postgres.is_empty(),
        "trace {trace_id} contains no wamn.postgres effect"
    );
    let topology = spans
        .iter()
        .filter(|span| span.span_context.trace_id().to_string() == trace_id)
        .map(|span| {
            format!(
                "{}:{}<-{}",
                span.name,
                span.span_context.span_id(),
                span.parent_span_id
            )
        })
        .collect::<Vec<_>>();
    assert!(
        postgres
            .iter()
            .all(|span| span_descends_from(spans, span, component)),
        "trace {trace_id} contains a PostgreSQL effect outside its component invocation: {topology:?}"
    );
}

fn assert_no_component_trace(spans: &[SpanData], trace_id: &str) {
    assert!(
        spans.iter().all(|span| {
            span.name != "wamn.component.invoke"
                || span.span_context.trace_id().to_string() != trace_id
        }),
        "refused trace {trace_id} reached a released component"
    );
}

async fn provision_journey_control(system_url: &str, admin: &Client) -> anyhow::Result<()> {
    reset_control_store(admin).await?;
    provision_org::run(ProvisionOrgArgs {
        org: ORG.to_owned(),
        template: TemplateArg::Trials,
        pool: "route-auth-pg18".to_owned(),
        system_database_url: Some(system_url.to_owned()),
        emit_clusters: None,
    })
    .await
    .context("stamp the journey org and environment policies through provision-org")
}

async fn install_journey_project(project: &Client, project_url: &str) -> anyhow::Result<()> {
    project
        .batch_execute(include_str!("../../../deploy/sql/catalog-schema.sql"))
        .await
        .context("install the catalog schema")?;
    project
        .batch_execute(include_str!("../../../deploy/sql/app-schema.sql"))
        .await
        .context("install the application authorization schema")?;
    apply_package::run(ApplyPackageArgs {
        package: package_root(),
        database_url: project_url.to_owned(),
        tenant: TENANT.to_owned(),
    })
    .await
    .context("apply the Receiving package through the exact-byte runner")?;
    Ok(())
}

async fn reconcile_journey_run_plane(system_url: &str, project_url: &str) -> anyhow::Result<()> {
    reconcile_run_plane::run(ReconcileRunPlaneArgs {
        system_database_url: system_url.to_owned(),
        admin_database_url: project_url.to_owned(),
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        tenant: TENANT.to_owned(),
        env: ENVIRONMENT.to_owned(),
        schema: "wamn_run".to_owned(),
        dry_run: false,
    })
    .await
    .context("reconcile the journey run plane")
}

async fn prepare_journey_credentials(
    system_url: &str,
    project_url: &str,
    root: &Path,
    host_secret_directory: &Path,
    host_secret_namespace: &str,
) -> anyhow::Result<JourneyCredentials> {
    async fn prepare(
        family: WorkloadRoleFamily,
        system_url: &str,
        target_url: Option<&str>,
        root: &Path,
        namespace: &str,
        name: &str,
    ) -> anyhow::Result<String> {
        let secret = root.join(format!("{name}.json"));
        let mut args = generation_args(family, system_url, target_url, &secret);
        args.namespace = namespace.to_owned();
        provision_project_env::run(args)
            .await
            .with_context(|| format!("prepare the production {name} generation"))?;
        secret_value(&secret, "url")
    }

    Ok(JourneyCredentials {
        guest_sql: prepare(
            WorkloadRoleFamily::App,
            system_url,
            Some(project_url),
            host_secret_directory,
            host_secret_namespace,
            "guest-sql",
        )
        .await?,
        executor_platform: prepare(
            WorkloadRoleFamily::ExecutorPlatform,
            system_url,
            Some(project_url),
            host_secret_directory,
            host_secret_namespace,
            "executor-platform",
        )
        .await?,
        http_admitter: prepare(
            WorkloadRoleFamily::HttpAdmitter,
            system_url,
            Some(project_url),
            host_secret_directory,
            host_secret_namespace,
            "http-admitter",
        )
        .await?,
        identity_reader: prepare(
            WorkloadRoleFamily::IdentityReader,
            system_url,
            None,
            host_secret_directory,
            host_secret_namespace,
            "identity-reader",
        )
        .await?,
        control_author: prepare(
            WorkloadRoleFamily::ControlAuthor,
            system_url,
            None,
            root,
            "wamn-system",
            "control-author",
        )
        .await?,
        management_admitter: prepare(
            WorkloadRoleFamily::ManagementAdmitter,
            system_url,
            Some(project_url),
            root,
            "wamn-system",
            "management-admitter",
        )
        .await?,
    })
}

fn render_component_declarations(root: &Path) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let output = root.join("component-declarations");
    std::fs::create_dir_all(&output).context("create rendered declaration directory")?;
    COMPONENTS
        .iter()
        .map(|(component, _)| {
            let source = publication_root()
                .join("components")
                .join(format!("{component}.json.in"));
            let rendered = std::fs::read_to_string(&source)
                .with_context(|| format!("read {}", source.display()))?
                .replace("__TENANT_ID__", TENANT);
            let destination = output.join(format!("{component}.json"));
            std::fs::write(&destination, rendered)
                .with_context(|| format!("write {}", destination.display()))?;
            Ok(((*component).to_owned(), destination))
        })
        .collect()
}

async fn push_journey_components(
    inputs: &JourneyInputs,
    project_url: &str,
    system_url: &str,
    declarations: &[(String, PathBuf)],
) -> anyhow::Result<()> {
    for (component, declaration) in declarations {
        push_component::run(PushComponentArgs {
            package: package_root(),
            component_bytes: inputs.component_directory.join(format!("{component}.wasm")),
            declaration: declaration.clone(),
            artifact_base: inputs.component_artifact_base.clone(),
            registry_auth_file: inputs.registry_auth_file.clone(),
            insecure_registry: true,
            admitted_platform_packages: vec!["wamn:node".to_owned(), "wamn:postgres".to_owned()],
            project_database_url: project_url.to_owned(),
            control_database_url: system_url.to_owned(),
        })
        .await
        .with_context(|| format!("publish production component {component}"))?;
    }
    Ok(())
}

async fn verify_journey_components_are_effectful(project: &Client) -> anyhow::Result<()> {
    let rows = project
        .query(
            "SELECT component, registered_operation, effects FROM catalog.component_library \
             WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
             ORDER BY component COLLATE \"C\"",
            &[&TENANT, &PACKAGE_ID, &PACKAGE_VERSION],
        )
        .await
        .context("read the six admitted Receiving effect projections")?;
    let expected = COMPONENTS
        .iter()
        .map(|(component, _)| (*component).to_owned())
        .collect::<BTreeSet<_>>();
    let observed = rows
        .iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        observed == expected,
        "component publication projected the wrong Receiving set: {observed:?}"
    );
    for row in rows {
        let component: String = row.get(0);
        let registered_operation: Option<String> = row.get(1);
        let operation = COMPONENTS
            .iter()
            .find_map(|(candidate, operation)| (*candidate == component).then_some(*operation))
            .with_context(|| format!("admission projected unknown component {component}"))?;
        let expected_operation = format!("{PACKAGE_ID}@{PACKAGE_VERSION}::{operation}");
        anyhow::ensure!(
            registered_operation.as_deref() == Some(expected_operation.as_str()),
            "Receiving component {component} projected the wrong operation: {registered_operation:?}"
        );
        let effects: Value = row.get(2);
        anyhow::ensure!(
            effects
                .as_array()
                .is_some_and(|effects| !effects.is_empty()),
            "Receiving component {component} is not effectful: {effects}"
        );
    }
    Ok(())
}

fn gate_document(command_id: &str, document: Value) -> Value {
    serde_json::json!({
        "document": "request",
        "body": {
            "schema-version": "0.1",
            "command-id": command_id,
            "command": {
                "kind": "gate",
                "input": {
                    "scope": {"project-id": PROJECT, "environment": ENVIRONMENT},
                    "package-id": PACKAGE_ID,
                    "package-version": PACKAGE_VERSION,
                    "document": document,
                },
            },
        },
    })
}

async fn gate_journey_wirings(bind: &str, bearer: &str) -> anyhow::Result<Vec<String>> {
    let client = reqwest::Client::new();
    let mut reports = Vec::with_capacity(COMPONENTS.len());
    for (component, _) in COMPONENTS {
        let path = publication_root()
            .join("wirings")
            .join(format!("{component}.json"));
        let document: Value = serde_json::from_slice(
            &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("parse {}", path.display()))?;
        let response = client
            .post(format!("http://{bind}/authoring"))
            .bearer_auth(bearer)
            .json(&gate_document(&format!("gate-{component}"), document))
            .send()
            .await
            .with_context(|| format!("submit {component} to the production Gate"))?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .with_context(|| format!("decode {component} Gate response"))?;
        anyhow::ensure!(
            status == reqwest::StatusCode::OK && body["body"]["outcome"]["status"] == "completed",
            "production Gate refused {component}: status={status} body={body}"
        );
        reports.push(
            body["body"]["outcome"]["value"]["result"]["report-id"]
                .as_str()
                .with_context(|| format!("production Gate returned no report id for {component}"))?
                .to_owned(),
        );
    }
    Ok(reports)
}

async fn verify_zero_case_gate_reports(
    control: &Client,
    report_ids: &[String],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        report_ids.len() == COMPONENTS.len(),
        "production Gate returned {} reports for {} wirings",
        report_ids.len(),
        COMPONENTS.len()
    );
    for report_id in report_ids {
        let row = control
            .query_one(
                "SELECT passed, summary FROM wamn_run.gate_reports \
                 WHERE tenant_id = $1 AND wiring_hash = $2",
                &[&TENANT, report_id],
            )
            .await
            .with_context(|| format!("read production Gate report {report_id}"))?;
        let passed: bool = row.get(0);
        let summary: Value = row.get(1);
        anyhow::ensure!(
            passed && summary == serde_json::json!({"cases": 0}),
            "production Gate report {report_id} was not an accepted zero-case judgment: {summary}"
        );
    }
    Ok(())
}

async fn author_journey_wirings(project_url: &str, system_url: &str) -> anyhow::Result<()> {
    for (component, _) in COMPONENTS {
        author_wiring::run(AuthorWiringArgs {
            database_url: project_url.to_owned(),
            control_database_url: system_url.to_owned(),
            tenant: TENANT.to_owned(),
            package_id: PACKAGE_ID.to_owned(),
            package_version: PACKAGE_VERSION.to_owned(),
            wiring_document: publication_root()
                .join("wirings")
                .join(format!("{component}.json")),
        })
        .await
        .with_context(|| format!("author gated wiring {component}"))?;
    }
    Ok(())
}

async fn publish_journey_release(
    inputs: &JourneyInputs,
    project_url: &str,
    system_url: &str,
    publisher: &str,
    project: &Client,
    control: &Client,
) -> anyhow::Result<(String, Arc<ReleaseManifestWeld>)> {
    let wirings = COMPONENTS
        .iter()
        .map(|(component, _)| {
            format!("{PACKAGE_ID}@{PACKAGE_VERSION}::{component}=1")
                .parse::<ReleaseWiringTarget>()
                .map_err(anyhow::Error::msg)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    publish_release::run(PublishReleaseArgs {
        database_url: project_url.to_owned(),
        control_database_url: system_url.to_owned(),
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        tenant: TENANT.to_owned(),
        effective_release_id: RELEASE_ID,
        environment: ENVIRONMENT.to_owned(),
        verified_publisher_principal: publisher.to_owned(),
        run_schema: "wamn_run".to_owned(),
        packages: vec![PackageCoordinate::new(PACKAGE_ID, PACKAGE_VERSION)?],
        wirings,
        attachments: publication_root().join("attachments.json"),
        route_host: Some(inputs.route_host.clone()),
        registrations: publication_root().join("registrations.json"),
    })
    .await
    .context("mint the production Receiving release")?;
    let inactive = control
        .query_opt(
            "SELECT deployed_manifest_hash FROM catalog.deployment_attestations \
             WHERE tenant_id = $1 AND effective_release_id = $2 \
               AND org_id = $3 AND project_id = $4 AND environment = $5",
            &[&TENANT, &(RELEASE_ID as i32), &ORG, &PROJECT, &ENVIRONMENT],
        )
        .await
        .context("verify the minted Receiving release remains inactive")?;
    anyhow::ensure!(
        inactive.is_none(),
        "minting the Receiving release activated it before deployment"
    );
    let digest: String = project
        .query_one(
            "SELECT manifest_digest FROM catalog.release_manifest_v3_snapshots \
             WHERE tenant_id = $1 AND effective_release_id = $2",
            &[&TENANT, &(RELEASE_ID as i32)],
        )
        .await
        .context("read the production-minted release digest")?
        .get(0);
    push_release_manifest::run(PushReleaseManifestArgs {
        database_url: project_url.to_owned(),
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        tenant: TENANT.to_owned(),
        effective_release_id: RELEASE_ID,
        artifact_base: inputs.release_artifact_base.clone(),
        registry_auth_file: inputs.registry_auth_file.clone(),
        insecure_registry: true,
        control_database_url: system_url.to_owned(),
    })
    .await
    .context("push and attest the production Receiving release")?;
    let serving: String = control
        .query_one(
            "SELECT deployed_manifest_hash FROM catalog.deployment_attestations \
             WHERE tenant_id = $1 AND effective_release_id = $2 \
               AND org_id = $3 AND project_id = $4 AND environment = $5",
            &[&TENANT, &(RELEASE_ID as i32), &ORG, &PROJECT, &ENVIRONMENT],
        )
        .await
        .context("verify the deployed Receiving release is serving")?
        .get(0);
    anyhow::ensure!(
        serving == digest,
        "serving attestation {serving} differs from minted release {digest}"
    );
    let source = ReleaseManifestSource::new(
        &inputs.release_artifact_base,
        true,
        &inputs.registry_auth_file,
    )
    .context("configure the release puller")?;
    let bytes = source
        .pull_verified(&digest)
        .await
        .context("pull the exact released manifest")?;
    let origin = format!("{}@{digest}", inputs.release_artifact_base);
    let release = Arc::new(
        ReleaseManifestWeld::load_canonical_bytes(&bytes, &origin)
            .context("weld the pulled Receiving release")?,
    );
    Ok((digest, release))
}

fn journey_postgres(credentials: &JourneyCredentials) -> anyhow::Result<Arc<WamnPostgres>> {
    let base = WamnPostgresConfig {
        credentials: None,
        guest_pool_max_size: 4,
        platform_pool_max_size: 4,
        wait_timeout_ms: 5_000,
        statement_timeout_ms: 10_000,
        row_limit: 10_000,
    };
    let configuration = serde_json::json!({
        PROJECT: {
            "credentials": {
                (AuthorityClass::GuestSql.as_str()): credentials.guest_sql,
                (AuthorityClass::ExecutorPlatform.as_str()): credentials.executor_platform,
                (AuthorityClass::CallableHttp.as_str()): credentials.http_admitter,
            }
        }
    });
    let projects = StaticCredentialProvider::projects_from_json(&configuration.to_string(), &base)?;
    let provider: Arc<dyn CredentialProvider> =
        Arc::new(StaticCredentialProvider::new(projects, None));
    Ok(Arc::new(WamnPostgres::with_provider(provider)))
}

fn released_component_digests(
    release: &ReleaseManifestWeld,
) -> anyhow::Result<HashMap<String, String>> {
    let digests = release
        .manifest()
        .components
        .iter()
        .filter(|component| component.package_id == PACKAGE_ID)
        .map(|component| {
            (
                component.component.clone(),
                component.digest.as_str().to_owned(),
            )
        })
        .collect::<HashMap<_, _>>();
    let expected = COMPONENTS
        .iter()
        .map(|(component, _)| *component)
        .collect::<BTreeSet<_>>();
    let observed = digests.keys().map(String::as_str).collect::<BTreeSet<_>>();
    anyhow::ensure!(
        observed == expected,
        "released manifest carries the wrong Receiving components: {observed:?}"
    );
    Ok(digests)
}

async fn build_journey_runtime(
    inputs: &JourneyInputs,
    credentials: &JourneyCredentials,
    release: Arc<ReleaseManifestWeld>,
) -> anyhow::Result<(
    Arc<wash_runtime::engine::Engine>,
    Component,
    Arc<FlowHttpRouting>,
    Arc<RouterDeliveryBridge>,
    tokio::task::JoinHandle<()>,
)> {
    let postgres = journey_postgres(credentials)?;
    let source = ComponentArtifactSource::new(
        ComponentArtifactSourceConfig::new(
            &inputs.component_artifact_base,
            true,
            REGISTRY_IO_TIMEOUT,
        )?
        .with_registry_auth_file(&inputs.registry_auth_file)?,
    );
    let engine = Arc::new(build_engine(&[]).context("build the Receiving router engine")?);
    let driver = Arc::new(RouterDriver::new(
        Arc::clone(&engine),
        Arc::clone(&postgres),
        Arc::new(WamnCredentials::empty()),
        Arc::new(WamnLogging::new(WamnLoggingConfig::default())?),
        Arc::from(Vec::<AllowedHost>::new()),
        Arc::clone(&release),
        source,
        RouterDriverConfig {
            owner_prefix: "receiving-route-live".to_owned(),
            project: PROJECT.to_owned(),
            schema: Some("receiving".to_owned()),
            cache_capacity: WiringCacheCapacity::default(),
        },
    )?);
    let jetstream = Arc::new(
        WamnJetstream::new(WamnJetstreamConfig { nats_url: None })
            .with_release(Some(Arc::clone(&release))),
    );
    let bridge = Arc::new(RouterDeliveryBridge::new(
        driver,
        Arc::clone(&release),
        jetstream,
        PROJECT,
    )?);
    let (identity_reader, identity_task) = connect(&credentials.identity_reader).await?;
    let routing = Arc::new(
        FlowHttpRouting::new(Some(release), RouteInFlightLimit::default()).with_authentication(
            Arc::new(RouteAuthentication::new(
                identity_reader,
                postgres,
                ORG,
                PROJECT,
                route_caller_subject(ORG, PROJECT, ENVIRONMENT)?,
            )),
        ),
    );
    let raw = engine.inner();
    let flow_http_bytes = std::fs::read(&inputs.flow_http_wasm)
        .with_context(|| format!("read {}", inputs.flow_http_wasm.display()))?;
    let flow_http = Component::new(raw, &flow_http_bytes)
        .map_err(|error| anyhow::anyhow!("compile flow-http: {error}"))?;
    Ok((engine, flow_http, routing, bridge, identity_task))
}

async fn invoke_journey_route(
    engine: &wash_runtime::engine::Engine,
    flow_http: &Component,
    routing: Arc<FlowHttpRouting>,
    bridge: Arc<RouterDeliveryBridge>,
    route_host: &str,
    path: &str,
    bearer: Option<&str>,
    traceparent: &str,
    body: Bytes,
) -> anyhow::Result<hyper::Response<Bytes>> {
    let raw = engine.inner();
    let mut linker = Linker::new(raw);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)
        .map_err(|error| anyhow::anyhow!("link WASI into flow-http: {error}"))?;
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)
        .map_err(|error| anyhow::anyhow!("link wasi:http into flow-http: {error}"))?;
    let loopback = Arc::new(std::sync::Mutex::new(
        wash_runtime::sockets::loopback::Network::default(),
    ));
    let mut workload = WorkloadComponent::new(
        "receiving-route-live",
        "receiving-route-live",
        "wamn",
        "flow-http",
        flow_http.clone(),
        linker,
        Vec::new(),
        LocalResources::default(),
        loopback,
        InstancePolicy::Ephemeral,
    );
    let imports = workload.world().imports;
    {
        let mut item = WorkloadItem::Component(&mut workload);
        routing
            .on_workload_item_bind(&mut item, WitInterfaces::new(&imports))
            .await
            .context("bind the released HTTP routing plugin")?;
        bridge
            .on_workload_item_bind(&mut item, WitInterfaces::new(&imports))
            .await
            .context("bind the production router-delivery bridge")?;
    }

    let mut plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> = HashMap::new();
    plugins.insert(FLOW_HTTP_ROUTING_ID, routing);
    plugins.insert(ROUTER_DELIVERY_ID, bridge);
    let workload_id = workload.workload_id().to_owned();
    let component_id = workload.id().to_owned();
    let ctx = Ctx::builder(workload_id, component_id)
        .with_plugins(plugins)
        .build();
    let mut store = Store::new(raw, SharedCtx::new(ctx));
    store.set_epoch_deadline(u64::MAX / 2);
    let compiled = workload.component().clone();
    let proxy = Proxy::instantiate_async(&mut store, &compiled, workload.linker())
        .await
        .map_err(|error| anyhow::anyhow!("instantiate shipped flow-http: {error}"))?;

    let body = Full::new(body).map_err(|never| -> ErrorCode { match never {} });
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(format!("http://{route_host}{path}"))
        .header("content-type", "application/json")
        .header("traceparent", traceparent);
    if let Some(bearer) = bearer {
        request = request.header("authorization", format!("Bearer {bearer}"));
    }
    let request = request
        .body(body)
        .context("build the Receiving HTTP request")?;
    let incoming = store
        .data_mut()
        .http()
        .new_incoming_request(Scheme::Http, request)
        .map_err(|error| anyhow::anyhow!("lower the Receiving HTTP request: {error}"))?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let out = store
        .data_mut()
        .http()
        .new_response_outparam(sender)
        .map_err(|error| anyhow::anyhow!("allocate the Receiving response outparam: {error}"))?;
    let call = wasmtime_wasi::runtime::spawn(async move {
        proxy
            .wasi_http_incoming_handler()
            .call_handle(&mut store, incoming, out)
            .await
            .map_err(|error| anyhow::anyhow!("call flow-http: {error}"))
    });
    let response = receiver
        .await
        .context("flow-http did not set its Receiving response")?
        .map_err(|error| anyhow::anyhow!("flow-http returned {error:?}"))?;
    let (parts, body) = response.into_parts();
    let body = body
        .collect()
        .await
        .context("collect the Receiving HTTP response")?;
    call.await.context("join flow-http")?;
    Ok(hyper::Response::from_parts(parts, body.to_bytes()))
}

fn successful_value(response: &hyper::Response<Bytes>, request_id: &str) -> anyhow::Result<Value> {
    anyhow::ensure!(
        response.status() == StatusCode::OK,
        "request {request_id} returned {}: {}",
        response.status(),
        String::from_utf8_lossy(response.body())
    );
    let body: Value = serde_json::from_slice(response.body())
        .with_context(|| format!("decode response for {request_id}"))?;
    let item = body
        .as_array()
        .filter(|items| items.len() == 1)
        .and_then(|items| items.first())
        .with_context(|| format!("request {request_id} returned a non-unit envelope: {body}"))?;
    anyhow::ensure!(
        item["request_id"] == request_id && item.get("error").is_none(),
        "request {request_id} returned a refusal or lost correlation: {item}"
    );
    item.get("value")
        .cloned()
        .with_context(|| format!("request {request_id} returned no value: {item}"))
}

async fn seed_receiving_business_rows(project: &Client) -> anyhow::Result<()> {
    project
        .batch_execute(
            "INSERT INTO receiving.item (id, item_number) VALUES \
               ('00000000-0000-0000-0000-000000000101', 'ITEM-101'); \
             INSERT INTO receiving.location (id, location_code) VALUES \
               ('00000000-0000-0000-0000-000000000201', 'DOCK-1'); \
             INSERT INTO receiving.purchase_order \
               (id, purchase_order_number, supplier_id, status, row_version, created_at, updated_at) \
             VALUES \
               ('00000000-0000-0000-0000-000000000301', 'PO-301', \
                '00000000-0000-0000-0000-000000000401', 'open', 1, \
                '2026-08-31T12:00:00.000000Z', '2026-08-31T12:00:00.000000Z'); \
             INSERT INTO receiving.purchase_order_line \
               (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity) \
             VALUES \
               ('00000000-0000-0000-0000-000000000501', \
                '00000000-0000-0000-0000-000000000301', 1, \
                '00000000-0000-0000-0000-000000000101', 5.0000, 0.0000);",
        )
        .await
        .context("seed only Receiving business rows")
}

#[tokio::test]
#[ignore = "requires disposable PG18 and authenticated OCI plus built virtualized Receiving and flow-http artifacts"]
async fn production_receiving_release_serves_all_six_pat_routes_with_correlated_traces()
-> anyhow::Result<()> {
    let system_url = required_journey(JOURNEY_URL_ENV)?;
    let inputs = JourneyInputs::required()?;
    let scratch = ScratchRoot::create()?;
    let root = scratch.path();
    let (admin, admin_task) = connect(&system_url).await?;
    let version: i32 = admin
        .query_one("SHOW server_version_num", &[])
        .await
        .context("read PostgreSQL version")?
        .get::<_, String>(0)
        .parse()
        .context("parse PostgreSQL version")?;
    anyhow::ensure!(
        version >= 180_000,
        "journey requires PostgreSQL 18 or newer"
    );

    provision_journey_control(&system_url, admin.as_ref()).await?;
    let management_secret = root.join("management-author-pat.json");
    let route =
        provision_route(&system_url, admin.as_ref(), root, Some(&management_secret)).await?;
    let route_caller_secret = root.join("route-caller-pat.json");
    std::fs::copy(&route_caller_secret, &inputs.route_caller_secret_output).with_context(|| {
        format!(
            "copy production-minted route-caller Secret from {} to {}",
            route_caller_secret.display(),
            inputs.route_caller_secret_output.display()
        )
    })?;
    std::fs::set_permissions(
        &inputs.route_caller_secret_output,
        Permissions::from_mode(0o600),
    )
    .with_context(|| {
        format!(
            "set route-caller Secret mode on {}",
            inputs.route_caller_secret_output.display()
        )
    })?;
    let (project, project_task) = connect(&route.database_url).await?;
    install_journey_project(project.as_ref(), &route.database_url).await?;
    reconcile_journey_run_plane(&system_url, &route.database_url).await?;
    let credentials = prepare_journey_credentials(
        &system_url,
        &route.database_url,
        root,
        &inputs.host_secret_directory,
        &inputs.host_secret_namespace,
    )
    .await?;
    reconcile_package_data_access::run(ReconcilePackageDataAccessArgs {
        packages: vec![package_root()],
        database_url: route.database_url.clone(),
        tenant: TENANT.to_owned(),
    })
    .await
    .context("reconcile the generated Receiving data privileges")?;
    let declarations = render_component_declarations(root)?;
    push_journey_components(&inputs, &route.database_url, &system_url, &declarations).await?;
    verify_journey_components_are_effectful(project.as_ref()).await?;

    let (readiness_tx, readiness_rx) = tokio::sync::oneshot::channel();
    let mut management_server = tokio::spawn(management::serve_with_readiness(
        ManagementServeArgs {
            bind: "127.0.0.1:0".to_owned(),
            system_url: credentials.identity_reader.clone(),
            control_authoring_database_url: credentials.control_author.clone(),
            management_admission_database_url: credentials.management_admitter.clone(),
            org: ORG.to_owned(),
            project: PROJECT.to_owned(),
            environment: ENVIRONMENT.to_owned(),
            tenant: TENANT.to_owned(),
            source_schema: "wamn_run".to_owned(),
        },
        readiness_tx,
    ));
    let management_bind = tokio::select! {
        ready = readiness_rx => ready
            .context("the production management Gate dropped readiness")?
            .to_string(),
        stopped = &mut management_server => {
            stopped.context("join the production management Gate")??;
            anyhow::bail!("the production management Gate stopped before listening");
        }
    };
    let gate_reports = gate_journey_wirings(
        &management_bind,
        route
            .management_token
            .as_deref()
            .context("project provisioning emitted no management-author PAT")?,
    )
    .await?;
    verify_zero_case_gate_reports(admin.as_ref(), &gate_reports).await?;
    author_journey_wirings(&route.database_url, &system_url).await?;
    reconcile_journey_run_plane(&system_url, &route.database_url).await?;
    let (_, release) = publish_journey_release(
        &inputs,
        &route.database_url,
        &system_url,
        route
            .management_principal_subject
            .as_deref()
            .context("project provisioning emitted no management-author principal")?,
        project.as_ref(),
        admin.as_ref(),
    )
    .await?;
    let component_digests = released_component_digests(&release)?;
    seed_receiving_business_rows(project.as_ref()).await?;

    let traces = TraceHarness::install();
    let (engine, flow_http, routing, bridge, identity_task) =
        build_journey_runtime(&inputs, &credentials, release).await?;
    let mut expected_traces = Vec::new();

    let (trace_id, traceparent) = journey_trace(1);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/purchase_order/get",
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"purchase-order-get","id":"00000000-0000-0000-0000-000000000301"}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "purchase-order-get")?;
    anyhow::ensure!(
        value["id"] == "00000000-0000-0000-0000-000000000301" && value["row_version"] == "1",
        "purchase_order.get returned the wrong row: {value}"
    );
    expected_traces.push((trace_id, "purchase_order_get"));

    let (trace_id, traceparent) = journey_trace(2);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/purchase_order/query",
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"purchase-order-query","filter":{"supplier_id":["00000000-0000-0000-0000-000000000401"],"status":["open"]},"sort":{"field":"created_at","direction":"ascending"},"limit":100}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "purchase-order-query")?;
    anyhow::ensure!(
        value["item"].as_array().is_some_and(|items| {
            items.len() == 1 && items[0]["id"] == "00000000-0000-0000-0000-000000000301"
        }),
        "purchase_order.query returned the wrong page: {value}"
    );
    expected_traces.push((trace_id, "purchase_order_query"));

    let (trace_id, traceparent) = journey_trace(3);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/purchase_order/update",
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"purchase-order-update","id":"00000000-0000-0000-0000-000000000301","expected_row_version":"1","change":{"supplier_id":"00000000-0000-0000-0000-000000000402"}}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "purchase-order-update")?;
    anyhow::ensure!(
        value["supplier_id"] == "00000000-0000-0000-0000-000000000402"
            && value["row_version"] == "2",
        "purchase_order.update returned the wrong row: {value}"
    );
    expected_traces.push((trace_id, "purchase_order_update"));

    let (trace_id, traceparent) = journey_trace(4);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/receiving/record_receipt",
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"record-receipt","value":{"idempotency_key":"receipt-command-1","purchase_order_id":"00000000-0000-0000-0000-000000000301","receipt_reference":"RECEIPT-1","occurred_at":"2026-08-31T12:30:00.000000Z","line":[{"purchase_order_line_id":"00000000-0000-0000-0000-000000000501","quantity":"5.0000","location_id":"00000000-0000-0000-0000-000000000201"}]}}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "record-receipt")?;
    anyhow::ensure!(
        value["purchase_order_status"] == "complete" && value["row_version"] == "3",
        "receiving.record_receipt returned the wrong command result: {value}"
    );
    let receipt_id = value["receipt_id"]
        .as_str()
        .context("record_receipt returned no receipt_id")?
        .to_owned();
    expected_traces.push((trace_id, "receiving_record_receipt"));

    let (trace_id, traceparent) = journey_trace(5);
    let receipt_get = serde_json::to_vec(&serde_json::json!([{
        "request_id": "receipt-get",
        "id": receipt_id,
    }]))?;
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/receipt/get",
        Some(&route.token),
        &traceparent,
        Bytes::from(receipt_get),
    )
    .await?;
    let value = successful_value(&response, "receipt-get")?;
    anyhow::ensure!(
        value["receipt_reference"] == "RECEIPT-1",
        "receipt.get returned the wrong receipt: {value}"
    );
    expected_traces.push((trace_id, "receipt_get"));

    let (trace_id, traceparent) = journey_trace(6);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/receipt/query",
        Some(&route.token),
        &traceparent,
        Bytes::from_static(br#"[{"request_id":"receipt-query","limit":100}]"#),
    )
    .await?;
    let value = successful_value(&response, "receipt-query")?;
    anyhow::ensure!(
        value["item"].as_array().is_some_and(|items| {
            items.len() == 1 && items[0]["receipt_reference"] == "RECEIPT-1"
        }),
        "receipt.query returned the wrong page: {value}"
    );
    expected_traces.push((trace_id, "receipt_query"));

    let (unauthorized_trace, unauthorized_parent) = journey_trace(7);
    let unauthorized = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/purchase_order/get",
        None,
        &unauthorized_parent,
        Bytes::from_static(
            br#"[{"request_id":"unauthorized","id":"00000000-0000-0000-0000-000000000301"}]"#,
        ),
    )
    .await?;
    anyhow::ensure!(
        unauthorized.status() == StatusCode::UNAUTHORIZED,
        "unauthenticated Receiving route returned {}: {}",
        unauthorized.status(),
        String::from_utf8_lossy(unauthorized.body())
    );

    let (oversized_trace, oversized_parent) = journey_trace(8);
    let oversized = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/purchase_order/get",
        Some(&route.token),
        &oversized_parent,
        Bytes::from(vec![b' '; RAW_BODY_LIMIT + 1]),
    )
    .await?;
    anyhow::ensure!(
        oversized.status() == StatusCode::PAYLOAD_TOO_LARGE
            && oversized
                .headers()
                .get(hyper::header::CONTENT_TYPE)
                .is_some_and(|value| value == "text/plain; charset=utf-8")
            && oversized.body().as_ref() == b"request body exceeds 1048576-byte limit\n",
        "oversized Receiving route returned {}: {}",
        oversized.status(),
        String::from_utf8_lossy(oversized.body())
    );

    let spans = traces.spans();
    for (trace_id, wiring_id) in expected_traces {
        let component_digest = component_digests
            .get(wiring_id)
            .with_context(|| format!("released component digest missing for {wiring_id}"))?;
        assert_route_trace(&spans, &trace_id, wiring_id, component_digest);
    }
    assert_no_component_trace(&spans, &unauthorized_trace);
    assert_no_component_trace(&spans, &oversized_trace);

    management_server.abort();
    identity_task.abort();
    project_task.abort();
    admin_task.abort();
    Ok(())
}
