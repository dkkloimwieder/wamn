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
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;
use tokio_postgres::Client;
use tracing_subscriber::layer::SubscriberExt as _;
use wamn_catalog::{
    AttachmentKind, ComponentOperationDependency, PackageCoordinate,
    SERVING_MANIFEST_FORMAT_VERSION,
};
use wamn_control_provision::{
    SystemReader, WorkloadRoleFamily, parse_system_reader_url, sql as provision_sql,
};
use wamn_ctl::apply_package::{self, ApplyPackageArgs};
use wamn_ctl::author_wiring::{self, AuthorWiringArgs};
use wamn_ctl::dev::DevSourceState;
use wamn_ctl::dev::watch::GitSource;
use wamn_ctl::provision_project_env;
use wamn_ctl::publish_release::{self, PublishReleaseArgs, ReleaseWiringTarget};
use wamn_ctl::push_component::{self, PushComponentArgs};
use wamn_ctl::push_release_manifest::{self, PushReleaseManifestArgs};
use wamn_ctl::reconcile_package_data_access::{self, ReconcilePackageDataAccessArgs};
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
use wamn_runtime::engine::{
    build_engine_with_host_memory_and_compilation_cache, default_host_memory_budgets,
};
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

use crate::dev_environment::{
    DevEnvironmentInputs, ENVIRONMENT, JourneyCredentials, ORG, PROJECT, RELEASE_ID, TENANT,
    clean_dev_verification_gate_roles, connect, generation_args, install_journey_platform_floor,
    prepare_journey_credentials, provision_journey_control, provision_route, read_json,
    reconcile_journey_run_plane, reset_control_store, secret_value, start_journey_management_gate,
    write_dev_config,
};

const URL_ENV: &str = "WAMN_ROUTE_AUTH_PG18_URL";
const JOURNEY_URL_ENV: &str = "WAMN_ROUTE_PG18_URL";
const DEV_COMMAND_TIMEOUT: Duration = Duration::from_secs(12 * 60);
const DEV_EXPECTED_MIGRATIONS: [(&str, &str, i32, &str); 3] = [
    (
        OVERLAY_PACKAGE_ID,
        OVERLAY_PACKAGE_VERSION,
        1,
        "migrations/0001_add_inspection_required.sql",
    ),
    (
        OVERLAY_PACKAGE_ID,
        OVERLAY_PACKAGE_VERSION,
        2,
        "migrations/0002_quality_inspection.sql",
    ),
    (
        BASE_PACKAGE_ID,
        BASE_PACKAGE_VERSION,
        1,
        "migrations/0001_initial.sql",
    ),
];
const OTHER_PROJECT: &str = "other";
const OTHER_ENVIRONMENT: &str = "prod";
const ROUTE_CALLER_ROLE: &str = "route-caller";
const ATTACHMENT_ID: &str = "receiving-purchase-order-get";
const OPERATION: &str = "wamn-receiving:purchase-order/get@1.0.0";
const RESIDUE: &str = "wamn-receiving:obsolete/operation@1.0.0";
const BASE_PACKAGE_ID: &str = "wamn_receiving";
const BASE_PACKAGE_VERSION: &str = "1.0.0";
const BASE_COMPONENT: &str = "receiving";
const OVERLAY_PACKAGE_ID: &str = "client_acme_receiving";
const OVERLAY_PACKAGE_VERSION: &str = "3.0.0";
const OVERLAY_COMPONENT: &str = "client_acme_receiving";
const RAW_BODY_LIMIT: usize = 1024 * 1024;
const REGISTRY_IO_TIMEOUT: Duration = Duration::from_secs(30);
const BASE_OPERATIONS: [(&str, &str); 8] = [
    ("location_list", "wamn-receiving:location/list@1.0.0"),
    (
        "purchase_order_get",
        "wamn-receiving:purchase-order/get@1.0.0",
    ),
    (
        "purchase_order_query",
        "wamn-receiving:purchase-order/query@1.0.0",
    ),
    (
        "purchase_order_update",
        "wamn-receiving:purchase-order/update@1.0.0",
    ),
    ("receipt_get", "wamn-receiving:receipt/get@1.0.0"),
    ("receipt_query", "wamn-receiving:receipt/query@1.0.0"),
    (
        "receiving_load_receipt_screen",
        "wamn-receiving:receiving/load-receipt-screen@1.0.0",
    ),
    (
        "receiving_record_receipt",
        "wamn-receiving:receiving/record-receipt@1.0.0",
    ),
];
const OVERLAY_OPERATIONS: [(&str, &str); 6] = [
    (
        "purchase_order_get",
        "client-acme-receiving:purchase-order/get@3.0.0",
    ),
    (
        "purchase_order_update",
        "client-acme-receiving:purchase-order/update@3.0.0",
    ),
    (
        "receiving_record_receipt",
        "client-acme-receiving:receiving/record-receipt@3.0.0",
    ),
    (
        "quality_load_purchase_order_detail",
        "client-acme-receiving:quality/load-purchase-order-detail@3.0.0",
    ),
    (
        "quality_approve_inspection",
        "client-acme-receiving:quality/approve-inspection@3.0.0",
    ),
    (
        "quality_create_inspection",
        "client-acme-receiving:quality/create-inspection@3.0.0",
    ),
];
const BASE_RECORD_RECEIPT: &str = "wamn-receiving:receiving/record-receipt@1.0.0";
const OVERLAY_RECORD_RECEIPT: &str = "client-acme-receiving:receiving/record-receipt@3.0.0";
const PREEXISTING_QUALITY_RECEIPT_ID: &str = "00000000-0000-0000-0000-000000000603";
const MATERIALIZER_STREAM: &str = "EVT_acme_dev";
const MATERIALIZER_DURABLE: &str =
    "mat_receiving-route-auth_client_acme_receiving_quality_create_inspection";

#[derive(Clone, Copy)]
struct JourneyAttachment {
    id: &'static str,
    package_id: &'static str,
    wiring_id: &'static str,
    path: &'static str,
    operation: &'static str,
}

// Deployment-owned route spellings live in this one publication table rather
// than leaking into operation or component identity.
const JOURNEY_ATTACHMENTS: [JourneyAttachment; 13] = [
    JourneyAttachment {
        id: "location-list-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "location_list",
        path: "/location/list",
        operation: "wamn-receiving:location/list@1.0.0",
    },
    JourneyAttachment {
        id: "purchase-order-get-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "purchase_order_get",
        path: "/purchase_order/get",
        operation: "wamn-receiving:purchase-order/get@1.0.0",
    },
    JourneyAttachment {
        id: "purchase-order-query-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "purchase_order_query",
        path: "/purchase_order/query",
        operation: "wamn-receiving:purchase-order/query@1.0.0",
    },
    JourneyAttachment {
        id: "purchase-order-update-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "purchase_order_update",
        path: "/purchase_order/update",
        operation: "wamn-receiving:purchase-order/update@1.0.0",
    },
    JourneyAttachment {
        id: "receipt-get-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "receipt_get",
        path: "/receipt/get",
        operation: "wamn-receiving:receipt/get@1.0.0",
    },
    JourneyAttachment {
        id: "receipt-query-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "receipt_query",
        path: "/receipt/query",
        operation: "wamn-receiving:receipt/query@1.0.0",
    },
    JourneyAttachment {
        id: "receiving-record-receipt-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "receiving_record_receipt",
        path: "/receiving/record_receipt",
        operation: BASE_RECORD_RECEIPT,
    },
    JourneyAttachment {
        id: "receiving-load-receipt-screen-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "receiving_load_receipt_screen",
        path: "/receiving/load_receipt_screen",
        operation: "wamn-receiving:receiving/load-receipt-screen@1.0.0",
    },
    JourneyAttachment {
        id: "client-acme-receiving-purchase-order-get-http",
        package_id: OVERLAY_PACKAGE_ID,
        wiring_id: "purchase_order_get",
        path: "/acme/purchase_order/get",
        operation: "client-acme-receiving:purchase-order/get@3.0.0",
    },
    JourneyAttachment {
        id: "client-acme-receiving-purchase-order-update-http",
        package_id: OVERLAY_PACKAGE_ID,
        wiring_id: "purchase_order_update",
        path: "/acme/purchase_order/update",
        operation: "client-acme-receiving:purchase-order/update@3.0.0",
    },
    JourneyAttachment {
        id: "client-acme-receiving-receiving-record-receipt-http",
        package_id: OVERLAY_PACKAGE_ID,
        wiring_id: "receiving_record_receipt",
        path: "/acme/receiving/record_receipt",
        operation: OVERLAY_RECORD_RECEIPT,
    },
    JourneyAttachment {
        id: "client-acme-receiving-quality-load-purchase-order-detail-http",
        package_id: OVERLAY_PACKAGE_ID,
        wiring_id: "quality_load_purchase_order_detail",
        path: "/acme/quality/load_purchase_order_detail",
        operation: "client-acme-receiving:quality/load-purchase-order-detail@3.0.0",
    },
    JourneyAttachment {
        id: "client-acme-receiving-quality-approve-inspection-http",
        package_id: OVERLAY_PACKAGE_ID,
        wiring_id: "quality_approve_inspection",
        path: "/acme/quality/approve_inspection",
        operation: "client-acme-receiving:quality/approve-inspection@3.0.0",
    },
];

#[derive(Clone, Copy)]
struct JourneyPackage {
    id: &'static str,
    version: &'static str,
    component: &'static str,
    operations: &'static [(&'static str, &'static str)],
}

const JOURNEY_PACKAGES: [JourneyPackage; 2] = [
    JourneyPackage {
        id: BASE_PACKAGE_ID,
        version: BASE_PACKAGE_VERSION,
        component: BASE_COMPONENT,
        operations: &BASE_OPERATIONS,
    },
    JourneyPackage {
        id: OVERLAY_PACKAGE_ID,
        version: OVERLAY_PACKAGE_VERSION,
        component: OVERLAY_COMPONENT,
        operations: &OVERLAY_OPERATIONS,
    },
];

#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    Authentication(u16, String),
    Permission(Box<str>),
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

fn scratch_root() -> PathBuf {
    std::env::temp_dir().join(format!("route-authentication-live-{}", std::process::id()))
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

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/receiving")
}

fn overlay_package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/client_acme_receiving")
}

fn journey_package_root(package: JourneyPackage) -> PathBuf {
    if package.id == BASE_PACKAGE_ID {
        package_root()
    } else {
        overlay_package_root()
    }
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
        8,
        "Receiving declares exactly eight operations"
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
            "component": BASE_COMPONENT,
            "interface-version": "0.1.0",
            "digest": format!("sha256:{}", "a".repeat(64)),
            "operations": {
                (OPERATION): {"registered-operation": OPERATION}
            }
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

async fn routing(
    identity_reader: Arc<Client>,
    postgres: Arc<WamnPostgres>,
    weld: Arc<ReleaseManifestWeld>,
) -> anyhow::Result<FlowHttpRouting> {
    Ok(
        FlowHttpRouting::new(Some(weld), RouteInFlightLimit::default()).with_authentication(
            Arc::new(
                RouteAuthentication::new(
                    identity_reader,
                    postgres,
                    ORG,
                    PROJECT,
                    route_caller_subject(ORG, PROJECT, ENVIRONMENT)?,
                )
                .await?,
            ),
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
    .await
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
    .await
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

/// The one process setting the cluster journey hands this crate: the path to
/// its input document. Everything else crosses as fields of that document.
const JOURNEY_DOCUMENT_ENV: &str = "WAMN_JOURNEY_DOCUMENT";
/// Checked-in schema generated from [`JourneyDocument`], relative to this
/// crate's manifest. Regenerate with the ignored test beside its drift test.
const JOURNEY_SCHEMA_PATH: &str = "schema/wamn-journey.schema.json";
/// A complete example the shell writer reproduces byte-for-byte and this crate
/// parses, so the two sides are pinned to one artifact rather than to each
/// other's reading of the schema.
const JOURNEY_EXAMPLE_PATH: &str = "schema/wamn-journey.example.json";

/// Sole field authority for the cluster journey's input document.
///
/// An environment variable carries a process setting; data crosses a boundary
/// as a declared, schema'd artifact. This document replaced thirteen
/// `WAMN_*` environment variables that had grown one name at a time, each
/// encoding whatever its author was thinking about -- the application, the
/// test, the database -- until two PG18 URLs sat one segment apart in a flat
/// namespace with nothing to say they were different things. As fields they
/// are `system_pg_url` and, in the materializer phase, `project_pg_url`, and
/// the question does not arise.
///
/// The shell writes it once, with `jq`, from the values it owns; this crate
/// reads it strictly. `deny_unknown_fields` is what makes it a contract: a key
/// the writer invents and the reader does not know fails here, not forty
/// minutes into a cluster run as an empty string.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct JourneyDocument {
    system_pg_url: String,
    component_directory: PathBuf,
    compilation_cache_directory: PathBuf,
    flow_http_wasm: PathBuf,
    component_artifact_base: String,
    release_artifact_base: String,
    pub(crate) route_host: String,
    registry_auth_file: PathBuf,
    host_secret_directory: PathBuf,
    host_secret_namespace: String,
    pub(crate) route_caller_secret_output: PathBuf,
    /// Known only after the route phase has provisioned the project
    /// environment and the materializer trigger has produced a receipt. The
    /// shell amends the document with it then; before that it is absent, and
    /// the materializer test refuses to run rather than read an empty string.
    materializer: Option<MaterializerPhase>,
    /// Known only once the released route is reachable from this machine and
    /// the fixture rows exist (wamn-362o.27). The shell amends it in; the
    /// runtime assertions refuse to run without it rather than guess an
    /// endpoint or a pallet.
    pub(crate) runtime: Option<RuntimePhase>,
}

/// The runtime-assertion phase: where the released route answers from this
/// machine, and the fixture the journey seeded, declared ONCE there and handed
/// over here so the test carries no second copy of it.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimePhase {
    /// The route's origin as reachable from the test -- a temporary NodePort
    /// on a kind node's docker-network address. The Host header still names
    /// the released route host.
    pub(crate) route_endpoint: String,
    /// The fixture pallet the contention moves.
    pub(crate) pallet_id: String,
    /// The fixture location it moves to.
    pub(crate) to_location_id: String,
}

/// The materializer phase's inputs: the project-environment database the
/// route phase provisioned, the event stream it subscribes to, and the receipt
/// the trigger produced. The NATS URL is known from the start, but the only
/// reader that needs it is this phase's, so it rides here rather than being a
/// required top-level field a route-only run would have to invent.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MaterializerPhase {
    project_pg_url: String,
    nats_url: String,
    receipt_id: String,
}

impl JourneyDocument {
    pub(crate) fn required() -> anyhow::Result<Self> {
        let path = required_journey_path(JOURNEY_DOCUMENT_ENV)?;
        let bytes =
            std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        parse_journey_document(&bytes)
            .with_context(|| format!("{} is not a valid journey document", path.display()))
    }

    /// Every scalar the document carries, named, so emptiness is refused with
    /// the field's name rather than surfacing as a path that does not exist.
    fn scalars(&self) -> [(&'static str, &str); 11] {
        fn path(value: &Path) -> &str {
            value.to_str().unwrap_or("")
        }
        [
            ("system_pg_url", &self.system_pg_url),
            ("component_directory", path(&self.component_directory)),
            (
                "compilation_cache_directory",
                path(&self.compilation_cache_directory),
            ),
            ("flow_http_wasm", path(&self.flow_http_wasm)),
            ("component_artifact_base", &self.component_artifact_base),
            ("release_artifact_base", &self.release_artifact_base),
            ("route_host", &self.route_host),
            ("registry_auth_file", path(&self.registry_auth_file)),
            ("host_secret_directory", path(&self.host_secret_directory)),
            ("host_secret_namespace", &self.host_secret_namespace),
            (
                "route_caller_secret_output",
                path(&self.route_caller_secret_output),
            ),
        ]
    }
}

/// Parse one strict journey document. Unknown keys, missing keys and empty
/// values are all refusals, each naming the field.
fn parse_journey_document(bytes: &[u8]) -> anyhow::Result<JourneyDocument> {
    let document: JourneyDocument = serde_json::from_slice(bytes)
        .context("journey document disagrees with its generated schema")?;
    for (field, value) in document.scalars() {
        anyhow::ensure!(!value.is_empty(), "journey document field {field} is empty");
    }
    if let Some(materializer) = &document.materializer {
        for (field, value) in [
            ("materializer.project_pg_url", &materializer.project_pg_url),
            ("materializer.nats_url", &materializer.nats_url),
            ("materializer.receipt_id", &materializer.receipt_id),
        ] {
            anyhow::ensure!(!value.is_empty(), "journey document field {field} is empty");
        }
    }
    if let Some(runtime) = &document.runtime {
        for (field, value) in [
            ("runtime.route_endpoint", &runtime.route_endpoint),
            ("runtime.pallet_id", &runtime.pallet_id),
            ("runtime.to_location_id", &runtime.to_location_id),
        ] {
            anyhow::ensure!(!value.is_empty(), "journey document field {field} is empty");
        }
    }
    Ok(document)
}

/// Byte-stable pretty JSON Schema generated from the strict document type.
fn journey_document_schema_bytes() -> Vec<u8> {
    let schema = serde_json::to_value(schemars::schema_for!(JourneyDocument))
        .expect("journey document schema serializes");
    let mut bytes = serde_json::to_vec_pretty(&schema).expect("journey document schema serializes");
    bytes.push(b'\n');
    bytes
}

#[test]
fn checked_in_journey_schema_matches_generated_bytes() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(JOURNEY_SCHEMA_PATH);
    let checked_in = std::fs::read(&path).expect("read the checked-in journey schema");
    assert_eq!(checked_in, journey_document_schema_bytes());
}

#[test]
#[ignore = "schema regeneration command only"]
fn regenerate_checked_in_journey_schema() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(JOURNEY_SCHEMA_PATH);
    std::fs::write(path, journey_document_schema_bytes())
        .expect("write the generated journey schema");
}

/// The generated schema and the strict parser share ONE field authority: a
/// field cannot exist in the parser and be absent from the schema, or the
/// reverse, and the schema says which fields a writer must supply.
#[test]
fn generated_journey_schema_and_strict_parser_share_one_field_authority() {
    let first = journey_document_schema_bytes();
    assert_eq!(first, journey_document_schema_bytes());
    assert_eq!(first.last(), Some(&b'\n'));

    let schema: serde_json::Value = serde_json::from_slice(&first).expect("parse the schema");
    assert_eq!(schema["additionalProperties"], false);
    let properties = schema["properties"].as_object().expect("object properties");
    let required: Vec<&str> = schema["required"]
        .as_array()
        .expect("required set")
        .iter()
        .map(|key| key.as_str().expect("required key"))
        .collect();
    // Every scalar the parser refuses-when-empty is a required property, and
    // the only property that is not required is the phase the shell amends in.
    let example = parse_journey_document(&example_document()).expect("example parses");
    for (field, _) in example.scalars() {
        assert!(properties.contains_key(field), "schema lacks {field}");
        assert!(required.contains(&field), "schema does not require {field}");
    }
    assert_eq!(properties.len(), example.scalars().len() + 2);
    for phase in ["materializer", "runtime"] {
        assert!(properties.contains_key(phase));
        assert!(!required.contains(&phase));
    }
    let runtime = &schema["definitions"]["RuntimePhase"];
    assert_eq!(runtime["additionalProperties"], false);
    assert_eq!(
        runtime["required"],
        serde_json::json!(["pallet_id", "route_endpoint", "to_location_id"])
    );
    let phase = &schema["definitions"]["MaterializerPhase"];
    assert_eq!(phase["additionalProperties"], false);
    assert_eq!(
        phase["required"],
        serde_json::json!(["nats_url", "project_pg_url", "receipt_id"])
    );

    // And the refusals name the field, so a forty-minute run does not fail
    // under a message about something else.
    let mut missing: serde_json::Value =
        serde_json::from_slice(&example_document()).expect("example is JSON");
    missing.as_object_mut().expect("object").remove("route_host");
    let error = parse_journey_document(&serde_json::to_vec(&missing).expect("serialize"))
        .expect_err("a missing required field is refused");
    assert!(format!("{error:#}").contains("route_host"), "{error:#}");

    let mut unknown: serde_json::Value =
        serde_json::from_slice(&example_document()).expect("example is JSON");
    unknown["route_hots"] = serde_json::json!("typo.localhost");
    let error = parse_journey_document(&serde_json::to_vec(&unknown).expect("serialize"))
        .expect_err("an unknown field is refused, not ignored");
    assert!(format!("{error:#}").contains("route_hots"), "{error:#}");

    let mut empty: serde_json::Value =
        serde_json::from_slice(&example_document()).expect("example is JSON");
    empty["host_secret_namespace"] = serde_json::json!("");
    let error = parse_journey_document(&serde_json::to_vec(&empty).expect("serialize"))
        .expect_err("an empty value is refused, not passed through");
    assert!(format!("{error:#}").contains("host_secret_namespace"), "{error:#}");

    let mut no_phase: serde_json::Value =
        serde_json::from_slice(&example_document()).expect("example is JSON");
    no_phase.as_object_mut().expect("object").remove("materializer");
    let parsed = parse_journey_document(&serde_json::to_vec(&no_phase).expect("serialize"))
        .expect("the materializer phase is optional until the shell amends it in");
    assert!(parsed.materializer.is_none());
}

/// The checked-in example is what the shell writer reproduces byte-for-byte;
/// parsing it here is what pins the two sides to one artifact.
#[test]
fn the_checked_in_example_document_parses_with_every_field() {
    let document = parse_journey_document(&example_document()).expect("example parses");
    assert_eq!(document.route_host, "example.localhost");
    assert_eq!(document.host_secret_namespace, "wamn-example-journey");
    assert_eq!(document.registry_auth_file, Path::new("/tmp/example/docker/config.json"));
    let phase = document.materializer.expect("the example carries the amended phase");
    assert_eq!(phase.receipt_id, "00000000-0000-0000-0000-00000000c0de");
    let runtime = document.runtime.expect("the example carries the runtime phase");
    assert_eq!(runtime.route_endpoint, "http://10.0.0.2:30999");
    assert_eq!(runtime.pallet_id, "00000000-0000-0000-0000-000000000301");
}

fn example_document() -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(JOURNEY_EXAMPLE_PATH))
        .expect("read the checked-in journey example")
}

struct DevJourneyInputs {
    wamn_binary: PathBuf,
    environment: DevEnvironmentInputs,
}

impl DevJourneyInputs {
    fn required() -> anyhow::Result<Self> {
        let inputs = Self {
            wamn_binary: required_journey_path("WAMN_RECEIVING_DEV_BIN")?,
            environment: DevEnvironmentInputs {
                host_binary: required_journey_path("WAMN_RECEIVING_DEV_HOST_BIN")?,
                nats_url: required_journey("WAMN_RECEIVING_DEV_NATS_URL")?,
                tempo_query_url: required_journey("WAMN_RECEIVING_DEV_TEMPO_QUERY_URL")?,
                otel_exporter_otlp_endpoint: required_journey(
                    "WAMN_RECEIVING_DEV_OTEL_EXPORTER_OTLP_ENDPOINT",
                )?,
                flow_http_workload_image: required_journey(
                    "WAMN_RECEIVING_DEV_FLOW_HTTP_WORKLOAD_IMAGE",
                )?,
                component_artifact_base: required_journey(
                    "WAMN_ROUTE_COMPONENT_ARTIFACT_BASE",
                )?,
                release_artifact_base: required_journey(
                    "WAMN_ROUTE_RELEASE_ARTIFACT_BASE",
                )?,
                route_host: required_journey("WAMN_ROUTE_HOST")?,
                registry_auth_file: required_journey_path(
                    "WAMN_ROUTE_REGISTRY_AUTH_FILE",
                )?,
                package_sources: vec![
                    package_root()
                        .canonicalize()
                        .context("resolve the base package root")?,
                ],
            },
        };
        anyhow::ensure!(
            inputs.wamn_binary.is_file(),
            "WAMN_RECEIVING_DEV_BIN does not name a built wamn binary"
        );
        anyhow::ensure!(
            inputs.environment.host_binary.is_file(),
            "WAMN_RECEIVING_DEV_HOST_BIN does not name a built wamn-host binary"
        );
        Ok(inputs)
    }
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

fn journey_publication_root(package: JourneyPackage) -> PathBuf {
    journey_package_root(package).join("publication")
}

fn journey_trace(index: u64) -> (String, String) {
    let trace_id = format!("{index:032x}");
    let span_id = format!("{index:016x}");
    (trace_id.clone(), format!("00-{trace_id}-{span_id}-01"))
}

fn overlay_route_path(wiring_id: &str) -> &'static str {
    JOURNEY_ATTACHMENTS
        .iter()
        .find_map(|attachment| {
            (attachment.package_id == OVERLAY_PACKAGE_ID && attachment.wiring_id == wiring_id)
                .then_some(attachment.path)
        })
        .expect("every public overlay wiring has one deployment route")
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

fn trace_component_invocations<'a>(spans: &'a [SpanData], trace_id: &str) -> Vec<&'a SpanData> {
    spans
        .iter()
        .filter(|span| {
            span.name == "wamn.component.invoke"
                && span.span_context.trace_id().to_string() == trace_id
        })
        .collect()
}

fn assert_invocation_identity(
    component: &SpanData,
    trace_id: &str,
    wiring_id: &str,
    operation: &str,
    component_digest: &str,
    caller_principal_id: &str,
) {
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
        span_attribute(component, "wamn.operation").as_deref(),
        Some(operation),
        "trace {trace_id} invoked a different operation"
    );
    assert_eq!(
        span_attribute(component, "wamn.caller_principal_id").as_deref(),
        Some(caller_principal_id),
        "trace {trace_id} did not preserve the originating caller"
    );
}

fn assert_postgres_descendants<'a>(
    spans: &'a [SpanData],
    trace_id: &str,
    ancestor: &SpanData,
) -> Vec<&'a SpanData> {
    let postgres = spans
        .iter()
        .filter(|span| {
            span.name == "wamn.postgres"
                && span.span_context.trace_id().to_string() == trace_id
                && span_descends_from(spans, span, ancestor)
        })
        .collect::<Vec<_>>();
    assert!(
        !postgres.is_empty(),
        "trace {trace_id} contains no PostgreSQL effect below the expected component invocation"
    );
    postgres
}

fn assert_direct_route_trace(
    spans: &[SpanData],
    trace_id: &str,
    wiring_id: &str,
    operation: &str,
    component_digest: &str,
    caller_principal_id: &str,
) {
    let components = trace_component_invocations(spans, trace_id);
    assert_eq!(
        components.len(),
        1,
        "trace {trace_id} must contain one released component invocation"
    );
    let component = components[0];
    assert_invocation_identity(
        component,
        trace_id,
        wiring_id,
        operation,
        component_digest,
        caller_principal_id,
    );
    assert_eq!(
        span_attribute(component, "wamn.node_id").as_deref(),
        Some("operation"),
        "trace {trace_id} invoked a different wiring node"
    );
    let postgres = assert_postgres_descendants(spans, trace_id, component);
    assert_eq!(
        postgres.len(),
        spans
            .iter()
            .filter(|span| {
                span.name == "wamn.postgres" && span.span_context.trace_id().to_string() == trace_id
            })
            .count(),
        "trace {trace_id} contains a PostgreSQL effect outside its component invocation"
    );
}

fn assert_nested_record_receipt_trace(
    spans: &[SpanData],
    trace_id: &str,
    overlay_digest: &str,
    base_digest: &str,
    caller_principal_id: &str,
) {
    let components = trace_component_invocations(spans, trace_id);
    assert_eq!(
        components.len(),
        2,
        "trace {trace_id} must contain overlay and pinned-base invocations"
    );
    let overlay = components
        .iter()
        .find(|span| {
            span_attribute(span, "wamn.operation").as_deref() == Some(OVERLAY_RECORD_RECEIPT)
        })
        .copied()
        .expect("overlay record_receipt invocation is present");
    let base = components
        .iter()
        .find(|span| span_attribute(span, "wamn.operation").as_deref() == Some(BASE_RECORD_RECEIPT))
        .copied()
        .expect("pinned base record_receipt invocation is present");
    assert_invocation_identity(
        overlay,
        trace_id,
        "receiving_record_receipt",
        OVERLAY_RECORD_RECEIPT,
        overlay_digest,
        caller_principal_id,
    );
    assert_invocation_identity(
        base,
        trace_id,
        "receiving_record_receipt",
        BASE_RECORD_RECEIPT,
        base_digest,
        caller_principal_id,
    );
    assert!(
        span_descends_from(spans, base, overlay),
        "trace {trace_id} did not parent the pinned-base invocation under the overlay invocation"
    );
    assert_postgres_descendants(spans, trace_id, base);
    assert_postgres_descendants(spans, trace_id, overlay);
}

fn assert_cold_nested_acquisition(
    spans: &[SpanData],
    trace_id: &str,
    overlay_digest: &str,
    base_digest: &str,
) {
    let mut evidence = Vec::new();
    for phase in [
        "wamn.component.pull",
        "wamn.component.compile",
        "wamn.component.linker_setup",
        "wamn.component.link",
        "wamn.component.instantiate",
    ] {
        let observed = spans
            .iter()
            .filter(|span| {
                span.name == phase && span.span_context.trace_id().to_string() == trace_id
            })
            .collect::<Vec<_>>();
        assert_eq!(
            observed.len(),
            2,
            "cold nested trace {trace_id} must prepare both exact components during {phase}"
        );
        let elapsed_ms = observed
            .iter()
            .filter_map(|span| span.end_time.duration_since(span.start_time).ok())
            .map(|duration| duration.as_millis())
            .sum::<u128>();
        evidence.push(format!("{phase}={elapsed_ms}ms"));
    }
    for phase in ["wamn.component.pull", "wamn.component.compile"] {
        let digests = spans
            .iter()
            .filter(|span| {
                span.name == phase && span.span_context.trace_id().to_string() == trace_id
            })
            .filter_map(|span| span_attribute(span, "wamn.component_digest"))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            digests,
            BTreeSet::from([overlay_digest.to_owned(), base_digest.to_owned()]),
            "cold nested trace {trace_id} prepared the wrong component during {phase}"
        );
    }
    println!("RECEIVING_COLD_NESTED_PHASES {}", evidence.join(" "));
}

fn assert_nested_permission_denial_trace(
    spans: &[SpanData],
    trace_id: &str,
    overlay_digest: &str,
    base_digest: &str,
    caller_principal_id: &str,
) {
    let components = trace_component_invocations(spans, trace_id);
    assert_eq!(
        components.len(),
        1,
        "trace {trace_id} reached a component after the nested permission refusal"
    );
    let overlay = components[0];
    assert_invocation_identity(
        overlay,
        trace_id,
        "receiving_record_receipt",
        OVERLAY_RECORD_RECEIPT,
        overlay_digest,
        caller_principal_id,
    );
    assert!(
        components.iter().all(|span| {
            span_attribute(span, "wamn.operation").as_deref() != Some(BASE_RECORD_RECEIPT)
                && span_attribute(span, "wamn.component_digest").as_deref() != Some(base_digest)
        }),
        "trace {trace_id} invoked the denied pinned-base operation"
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

async fn install_journey_project(project: &Client, project_url: &str) -> anyhow::Result<()> {
    install_journey_platform_floor(project).await?;
    for package in JOURNEY_PACKAGES {
        apply_package::run(ApplyPackageArgs {
            package: journey_package_root(package),
            database_url: project_url.to_owned(),
            tenant: TENANT.to_owned(),
        })
        .await
        .with_context(|| {
            format!(
                "apply {}@{} through the exact-byte runner",
                package.id, package.version
            )
        })?;
    }
    Ok(())
}

async fn reconcile_journey_data_access(project_url: &str) -> anyhow::Result<()> {
    let packages = JOURNEY_PACKAGES
        .iter()
        .map(|package| journey_package_root(*package))
        .collect::<Vec<_>>();
    reconcile_package_data_access::reconcile_package_data_access(ReconcilePackageDataAccessArgs {
        packages: packages.clone(),
        database_url: project_url.to_owned(),
        tenant: TENANT.to_owned(),
    })
    .await
    .context("converge the fresh installed-set data-access union")?;
    let again = reconcile_package_data_access::reconcile_package_data_access(
        ReconcilePackageDataAccessArgs {
            packages,
            database_url: project_url.to_owned(),
            tenant: TENANT.to_owned(),
        },
    )
    .await
    .context("replay the installed-set data-access union")?;
    anyhow::ensure!(
        again.is_noop(),
        "installed-set data-access reconciliation did not converge"
    );
    Ok(())
}

async fn verify_journey_operation_grants(project: &Client) -> anyhow::Result<()> {
    let observed = project
        .query(
            "SELECT permission FROM app_system.permissions \
             WHERE tenant_id = $1 AND role_name = $2 \
             ORDER BY permission COLLATE \"C\"",
            &[&TENANT, &ROUTE_CALLER_ROLE],
        )
        .await
        .context("read the installed two-package operation-grant union")?
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<BTreeSet<_>>();
    let expected = BASE_OPERATIONS
        .iter()
        .chain(OVERLAY_OPERATIONS.iter())
        .map(|(_, token)| (*token).to_owned())
        .filter(|token| token != "client-acme-receiving:quality/create-inspection@3.0.0")
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        observed == expected,
        "installed packages projected the wrong route-caller grant union: {observed:?}"
    );
    Ok(())
}

fn repository_root() -> anyhow::Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .context("resolve the repository root for the product command")
}

async fn run_dev_product_command(
    inputs: &DevJourneyInputs,
    config: &Path,
) -> anyhow::Result<std::process::Output> {
    let mut command = Command::new(&inputs.wamn_binary);
    command
        .current_dir(repository_root()?)
        .args(["dev", "--config"])
        .arg(config)
        .arg("--overlay-root")
        .arg(
            overlay_package_root()
                .canonicalize()
                .context("resolve the overlay package root")?,
        )
        .kill_on_drop(true);
    tokio::time::timeout(DEV_COMMAND_TIMEOUT, command.output())
        .await
        .context("wamn dev exceeded its twelve-minute product bound")?
        .context("run the literal wamn dev product command")
}

fn verify_dev_command_receipt(output: &std::process::Output) -> anyhow::Result<()> {
    let stdout = String::from_utf8(output.stdout.clone()).context("wamn dev stdout is UTF-8")?;
    let stderr = String::from_utf8(output.stderr.clone()).context("wamn dev stderr is UTF-8")?;
    anyhow::ensure!(
        output.status.success(),
        "wamn dev failed with {}: stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
    let stages = wamn_ctl::dev::DEV_STAGE_ORDER
        .iter()
        .map(|stage| stage.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let expected = format!("run completed: {stages}");
    let receipts = stdout
        .lines()
        .filter(|line| line.starts_with("run completed:"))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        receipts == [expected.as_str()],
        "wamn dev returned the wrong product receipt: {receipts:?}; stdout={stdout:?}"
    );
    // The endpoint the activated release served, read off the public dev seam.
    // Only the Activate stage can publish it, so dropping that publish leaves
    // this line absent — which is the gate wamn-10yt.10.28 asked for, and it
    // cannot be satisfied by anything the command knows before it activates.
    let served = stdout
        .lines()
        .filter(|line| line.starts_with("run served: "))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        served.len() == 1,
        "wamn dev reported no single served endpoint: {served:?}; stdout={stdout:?}"
    );
    let served = served[0].trim_start_matches("run served: ");
    let (base_url, route_host) = served
        .split_once(" host=")
        .with_context(|| format!("the served line names a host: {served:?}"))?;
    anyhow::ensure!(
        base_url.starts_with("http://127.0.0.1:")
            && base_url
                .trim_start_matches("http://127.0.0.1:")
                .parse::<u16>()
                .is_ok_and(|port| port != 0),
        "the served endpoint is not a loopback port the host actually bound: {base_url:?}"
    );
    anyhow::ensure!(
        route_host == required_journey("WAMN_ROUTE_HOST")?,
        "the served route host is not the deployment-owned one: {route_host:?}"
    );
    Ok(())
}

fn declared_dev_data_access_grants() -> anyhow::Result<BTreeSet<(String, String, String, String)>> {
    let mut expected = BTreeSet::new();
    for package in JOURNEY_PACKAGES {
        let path = journey_package_root(package).join("generated/platform-policy/data-access.json");
        let policy = read_json(&path)?;
        anyhow::ensure!(
            policy["role"] == "wamn_app",
            "{} does not declare the shared package application role",
            path.display()
        );
        let relations = policy["relations"]
            .as_array()
            .with_context(|| format!("{} carries a relations array", path.display()))?;
        for relation in relations {
            let schema = relation["schema"]
                .as_str()
                .context("data-access relation carries schema")?;
            let table = relation["table"]
                .as_str()
                .context("data-access relation carries table")?;
            for (field, privilege) in [
                ("select_fields", "SELECT"),
                ("insert_fields", "INSERT"),
                ("update_fields", "UPDATE"),
            ] {
                for column in relation[field]
                    .as_array()
                    .with_context(|| format!("{schema}.{table} carries {field}"))?
                {
                    expected.insert((
                        schema.to_owned(),
                        table.to_owned(),
                        column
                            .as_str()
                            .with_context(|| format!("{schema}.{table} {field} is a string"))?
                            .to_owned(),
                        privilege.to_owned(),
                    ));
                }
            }
            if relation["lock"].as_bool() == Some(true) {
                expected.insert((
                    schema.to_owned(),
                    table.to_owned(),
                    relation["lock_update_field"]
                        .as_str()
                        .with_context(|| format!("{schema}.{table} lock carries its carrier"))?
                        .to_owned(),
                    "UPDATE".to_owned(),
                ));
            }
        }
    }
    Ok(expected)
}

async fn verify_dev_target_package_and_acl_state(project: &Client) -> anyhow::Result<()> {
    let packages = project
        .query(
            "SELECT package_id, package_version, manifest_sha256 \
             FROM catalog.packages WHERE tenant_id = $1 \
             ORDER BY package_id COLLATE \"C\", package_version COLLATE \"C\"",
            &[&TENANT],
        )
        .await
        .context("read product-command target package coordinates")?;
    let observed_packages = packages
        .iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, String>(2),
            )
        })
        .collect::<Vec<_>>();
    let expected_packages = [
        (OVERLAY_PACKAGE_ID, OVERLAY_PACKAGE_VERSION),
        (BASE_PACKAGE_ID, BASE_PACKAGE_VERSION),
    ];
    anyhow::ensure!(
        observed_packages.len() == expected_packages.len()
            && observed_packages.iter().zip(expected_packages).all(
                |((id, version, hash), expected)| {
                    (id.as_str(), version.as_str()) == expected
                        && hash.len() == 71
                        && hash.starts_with("sha256:")
                }
            ),
        "wamn dev installed the wrong exact package coordinates: {observed_packages:?}"
    );

    let migrations = project
        .query(
            "SELECT package_id, package_version, ordinal, relative_path, sha256 \
             FROM catalog.package_migrations WHERE tenant_id = $1 \
             ORDER BY package_id COLLATE \"C\", package_version COLLATE \"C\", ordinal",
            &[&TENANT],
        )
        .await
        .context("read product-command target migration ledgers")?;
    let observed_migrations = migrations
        .iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, i32>(2),
                row.get::<_, String>(3),
                row.get::<_, String>(4),
            )
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        observed_migrations.len() == DEV_EXPECTED_MIGRATIONS.len()
            && observed_migrations.iter().zip(DEV_EXPECTED_MIGRATIONS).all(
                |((id, version, ordinal, path, hash), expected)| {
                    (id.as_str(), version.as_str(), *ordinal, path.as_str()) == expected
                        && hash.len() == 71
                        && hash.starts_with("sha256:")
                }
            ),
        "wamn dev installed the wrong exact migration ledgers: {observed_migrations:?}"
    );

    let observed_permissions = project
        .query(
            "SELECT permission FROM app_system.permissions \
             WHERE tenant_id = $1 AND role_name = $2 \
             ORDER BY permission COLLATE \"C\"",
            &[&TENANT, &ROUTE_CALLER_ROLE],
        )
        .await
        .context("read product-command operation grants")?
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<BTreeSet<_>>();
    let expected_permissions = BASE_OPERATIONS
        .iter()
        .map(|(_, token)| (*token).to_owned())
        .chain(
            OVERLAY_OPERATIONS
                .iter()
                .map(|(_, token)| (*token).to_owned())
                .filter(|token| token != "client-acme-receiving:quality/create-inspection@3.0.0"),
        )
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        observed_permissions == expected_permissions,
        "wamn dev installed the wrong exact operation-grant union: {observed_permissions:?}"
    );

    let observed_column_grants = project
        .query(
            "SELECT table_schema::text, table_name::text, column_name::text, privilege_type::text \
             FROM information_schema.column_privileges \
             WHERE grantee = 'wamn_app' AND table_schema = 'receiving' \
             ORDER BY table_schema COLLATE \"C\", table_name COLLATE \"C\", \
                      column_name COLLATE \"C\", privilege_type COLLATE \"C\"",
            &[],
        )
        .await
        .context("read the installed package data-access union")?
        .into_iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, String>(2),
                row.get::<_, String>(3),
            )
        })
        .collect::<BTreeSet<_>>();
    let expected_column_grants = declared_dev_data_access_grants()?;
    anyhow::ensure!(
        observed_column_grants == expected_column_grants,
        "wamn dev installed the wrong exact column-privilege union: {observed_column_grants:?}"
    );
    let table_grant_count: i64 = project
        .query_one(
            "SELECT count(*) FROM information_schema.table_privileges \
             WHERE grantee = 'wamn_app' AND table_schema = 'receiving'",
            &[],
        )
        .await
        .context("read package application table-level privilege residue")?
        .get(0);
    anyhow::ensure!(
        table_grant_count == 0,
        "wamn dev left {table_grant_count} table-level package privilege grants"
    );
    let schema_usage: bool = project
        .query_one(
            "SELECT pg_catalog.has_schema_privilege('wamn_app', 'receiving', 'USAGE')",
            &[],
        )
        .await
        .context("read package application schema usage")?
        .get(0);
    anyhow::ensure!(schema_usage, "wamn dev omitted receiving schema USAGE");
    Ok(())
}

async fn verify_dev_release_state(
    control: &Client,
    inputs: &DevEnvironmentInputs,
    expected_source_commit: &str,
    expected_publisher_id: &str,
    expected_publisher_subject: &str,
) -> anyhow::Result<()> {
    let release = control
        .query_one(
            "SELECT environment, verified_publisher_principal \
             FROM catalog.effective_releases \
             WHERE tenant_id = $1 AND effective_release_id = $2",
            &[&TENANT, &(RELEASE_ID as i32)],
        )
        .await
        .context("read the product-command effective release")?;
    let environment: String = release.get(0);
    let publisher: Option<String> = release.get(1);
    anyhow::ensure!(
        environment == ENVIRONMENT && publisher.is_none(),
        "wamn dev projected more than the control-plane release identity: \
         environment={environment:?} publisher={publisher:?}"
    );

    let attestation = control
        .query_one(
            "SELECT deployed_manifest_hash, source_commit \
             FROM catalog.deployment_attestations \
             WHERE tenant_id = $1 AND effective_release_id = $2 \
               AND org_id = $3 AND project_id = $4 AND environment = $5",
            &[&TENANT, &(RELEASE_ID as i32), &ORG, &PROJECT, &ENVIRONMENT],
        )
        .await
        .context("read the product-command deployment attestation")?;
    let manifest_hash: String = attestation.get(0);
    let source_commit: String = attestation.get(1);
    anyhow::ensure!(
        manifest_hash.len() == 71 && manifest_hash.starts_with("sha256:"),
        "wamn dev recorded a malformed release attestation: {manifest_hash}"
    );
    anyhow::ensure!(
        source_commit == expected_source_commit,
        "wamn dev attested source commit {source_commit:?}, expected {expected_source_commit:?}"
    );

    let source = ReleaseManifestSource::new(
        &inputs.release_artifact_base,
        true,
        &inputs.registry_auth_file,
    )
    .context("configure the product-command release puller")?;
    let bytes = source
        .pull_verified(&manifest_hash)
        .await
        .context("pull the exact product-command release manifest")?;
    let origin = format!("{}@{manifest_hash}", inputs.release_artifact_base);
    let release = ReleaseManifestWeld::load_canonical_bytes(&bytes, &origin)
        .context("weld the product-command release manifest")?;
    let expected_packages = JOURNEY_PACKAGES
        .iter()
        .map(|package| PackageCoordinate::new(package.id, package.version))
        .collect::<Result<BTreeSet<_>, _>>()?;
    anyhow::ensure!(
        release.manifest().release.tenant_id == TENANT
            && release.manifest().release.effective_release_id.get() == RELEASE_ID
            && release.manifest().release.environment == ENVIRONMENT
            && release.manifest().release.packages == expected_packages,
        "wamn dev published the wrong exact release closure: {:?}",
        release.manifest().release
    );

    let publish_audits = control
        .query(
            "SELECT principal_id, principal_subject, effective_role, provenance_commit, \
                    provenance_dirty \
             FROM catalog.authoring_command_audit \
             WHERE tenant_id = $1 AND command_kind = 'publish' \
               AND org = $2 AND project = $3 AND environment = $4 \
             ORDER BY command_id COLLATE \"C\"",
            &[&TENANT, &ORG, &PROJECT, &ENVIRONMENT],
        )
        .await
        .context("read the product-command Publish audit")?;
    let expected_publish_count = JOURNEY_PACKAGES
        .iter()
        .map(|package| package.operations.len())
        .sum::<usize>();
    anyhow::ensure!(
        publish_audits.len() == expected_publish_count,
        "wamn dev recorded {} Publish audits, expected {expected_publish_count}",
        publish_audits.len()
    );
    for audit in publish_audits {
        let principal_id: String = audit.get(0);
        let principal_subject: String = audit.get(1);
        let effective_role: String = audit.get(2);
        let provenance_commit: Option<String> = audit.get(3);
        let provenance_dirty: Option<bool> = audit.get(4);
        anyhow::ensure!(
            principal_id == expected_publisher_id
                && principal_subject == expected_publisher_subject
                && effective_role == "project-author"
                && provenance_commit.as_deref() == Some(expected_source_commit)
                && provenance_dirty == Some(false),
            "wamn dev Publish audit carried the wrong publisher or provenance: \
             principal_id={principal_id:?} principal_subject={principal_subject:?} \
             effective_role={effective_role:?} provenance_commit={provenance_commit:?} \
             provenance_dirty={provenance_dirty:?}"
        );
    }
    Ok(())
}

async fn current_database_acl(client: &Client) -> anyhow::Result<(String, Option<String>)> {
    let row = client
        .query_one(
            "SELECT datname::text, datacl::text FROM pg_catalog.pg_database \
             WHERE datname = current_database()",
            &[],
        )
        .await
        .context("read the current database ACL sentinel")?;
    Ok((row.get(0), row.get(1)))
}

async fn verify_dev_verification_database_absent(
    admin: &Client,
    database: &str,
) -> anyhow::Result<()> {
    let present: bool = admin
        .query_one(
            "SELECT EXISTS (SELECT FROM pg_catalog.pg_database WHERE datname = $1)",
            &[&database],
        )
        .await
        .context("read disposable verification database cleanup")?
        .get(0);
    anyhow::ensure!(
        !present,
        "wamn dev left disposable verification database {database} behind"
    );
    Ok(())
}

struct JourneyComponentDeclaration {
    package: JourneyPackage,
    path: PathBuf,
}

fn render_component_declarations(root: &Path) -> anyhow::Result<Vec<JourneyComponentDeclaration>> {
    let output = root.join("component-declarations");
    std::fs::create_dir_all(&output).context("create rendered declaration directory")?;
    JOURNEY_PACKAGES
        .into_iter()
        .map(|package| {
            let source = journey_publication_root(package)
                .join("components")
                .join(format!("{}.json.in", package.component));
            let mut declaration: Value = serde_json::from_slice(
                &std::fs::read(&source).with_context(|| format!("read {}", source.display()))?,
            )
            .with_context(|| format!("parse {}", source.display()))?;
            declaration["scope"]["tenant-id"] = Value::String(TENANT.to_owned());
            let destination = output.join(format!("{}.json", package.component));
            std::fs::write(&destination, serde_json::to_vec(&declaration)?)
                .with_context(|| format!("write {}", destination.display()))?;
            Ok(JourneyComponentDeclaration {
                package,
                path: destination,
            })
        })
        .collect()
}

async fn push_journey_components(
    inputs: &JourneyDocument,
    project_url: &str,
    system_url: &str,
    declarations: &[JourneyComponentDeclaration],
) -> anyhow::Result<()> {
    for declaration in declarations {
        let package = declaration.package;
        push_component::run(PushComponentArgs {
            package: journey_package_root(package),
            component_bytes: inputs
                .component_directory
                .join(format!("{}.wasm", package.component)),
            declaration: declaration.path.clone(),
            artifact_base: inputs.component_artifact_base.clone(),
            registry_auth_file: inputs.registry_auth_file.clone(),
            insecure_registry: true,
            admitted_platform_packages: vec!["wamn:node".to_owned(), "wamn:postgres".to_owned()],
            project_database_url: project_url.to_owned(),
            control_database_url: system_url.to_owned(),
        })
        .await
        .with_context(|| {
            format!(
                "publish production component {}@{}::{}",
                package.id, package.version, package.component
            )
        })?;
    }
    Ok(())
}

async fn verify_journey_components_are_effectful(
    project: &Client,
) -> anyhow::Result<HashMap<String, String>> {
    let mut digests = HashMap::new();
    for package in JOURNEY_PACKAGES {
        let rows = project
            .query(
                "SELECT component, operations, effects, component_digest \
                 FROM catalog.component_library \
                 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
                 ORDER BY component COLLATE \"C\"",
                &[&TENANT, &package.id, &package.version],
            )
            .await
            .with_context(|| format!("read the admitted {} effect projection", package.id))?;
        anyhow::ensure!(
            rows.len() == 1,
            "component publication projected {} {} rows instead of one",
            rows.len(),
            package.id
        );
        let row = &rows[0];
        let component: String = row.get(0);
        anyhow::ensure!(
            component == package.component,
            "component publication projected {component} instead of {}",
            package.component
        );
        let operations: Value = row.get(1);
        let operation_facts = operations
            .as_object()
            .with_context(|| format!("{} operations fact is not an object", package.id))?;
        let expected = package
            .operations
            .iter()
            .map(|(_, token)| *token)
            .collect::<BTreeSet<_>>();
        let observed = operation_facts
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        anyhow::ensure!(
            observed == expected,
            "{} component projected the wrong operation set: {observed:?}",
            package.id
        );
        for token in expected {
            let fact = operation_facts
                .get(token)
                .with_context(|| format!("{} operation fact missing for {token}", package.id))?;
            let registered = fact["registered-operation"].as_str();
            if token == "client-acme-receiving:quality/create-inspection@3.0.0" {
                anyhow::ensure!(
                    registered.is_none(),
                    "private operation {token} fabricated an authorization identity"
                );
            } else {
                anyhow::ensure!(
                    registered == Some(token),
                    "operation {token} projected a different authorization identity"
                );
            }
        }
        let effects: Value = row.get(2);
        anyhow::ensure!(
            effects
                .as_array()
                .is_some_and(|effects| !effects.is_empty()),
            "{} component is not effectful: {effects}",
            package.id
        );
        let digest: String = row.get(3);
        anyhow::ensure!(
            digests.insert(package.id.to_owned(), digest).is_none(),
            "{} projected more than one component digest",
            package.id
        );
    }
    Ok(digests)
}

fn gate_document(command_id: &str, package: JourneyPackage, document: Value) -> Value {
    serde_json::json!({
        "document": "request",
        "body": {
            "schema-version": "0.1",
            "command-id": command_id,
            "command": {
                "kind": "gate",
                "input": {
                    "scope": {"project-id": PROJECT, "environment": ENVIRONMENT},
                    "package-id": package.id,
                    "package-version": package.version,
                    "document": document,
                },
            },
        },
    })
}

async fn gate_journey_wirings(bind: &str, bearer: &str) -> anyhow::Result<Vec<String>> {
    let client = reqwest::Client::new();
    let mut reports = Vec::with_capacity(
        JOURNEY_PACKAGES
            .iter()
            .map(|package| package.operations.len())
            .sum(),
    );
    for package in JOURNEY_PACKAGES {
        for (wiring, _) in package.operations {
            let path = journey_publication_root(package)
                .join("wirings")
                .join(format!("{wiring}.json"));
            let document: Value = serde_json::from_slice(
                &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
            )
            .with_context(|| format!("parse {}", path.display()))?;
            let response = client
                .post(format!("http://{bind}/authoring"))
                .bearer_auth(bearer)
                .json(&gate_document(
                    &format!("gate-{}-{wiring}", package.id),
                    package,
                    document,
                ))
                .send()
                .await
                .with_context(|| {
                    format!("submit {}::{wiring} to the production Gate", package.id)
                })?;
            let status = response.status();
            let body: Value = response
                .json()
                .await
                .with_context(|| format!("decode {}::{wiring} Gate response", package.id))?;
            anyhow::ensure!(
                status == reqwest::StatusCode::OK
                    && body["body"]["outcome"]["status"] == "completed",
                "production Gate refused {}::{wiring}: status={status} body={body}",
                package.id
            );
            reports.push(
                body["body"]["outcome"]["value"]["result"]["report-id"]
                    .as_str()
                    .with_context(|| {
                        format!(
                            "production Gate returned no report id for {}::{wiring}",
                            package.id
                        )
                    })?
                    .to_owned(),
            );
        }
    }
    Ok(reports)
}

async fn verify_zero_case_gate_reports(
    control: &Client,
    report_ids: &[String],
) -> anyhow::Result<()> {
    let expected_count = JOURNEY_PACKAGES
        .iter()
        .map(|package| package.operations.len())
        .sum::<usize>();
    anyhow::ensure!(
        report_ids.len() == expected_count,
        "production Gate returned {} reports for {expected_count} wirings",
        report_ids.len()
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
    for package in JOURNEY_PACKAGES {
        for (wiring, _) in package.operations {
            author_wiring::run(AuthorWiringArgs {
                database_url: project_url.to_owned(),
                control_database_url: system_url.to_owned(),
                tenant: TENANT.to_owned(),
                package_id: package.id.to_owned(),
                package_version: package.version.to_owned(),
                wiring_document: journey_publication_root(package)
                    .join("wirings")
                    .join(format!("{wiring}.json")),
            })
            .await
            .with_context(|| format!("author gated wiring {}::{wiring}", package.id))?;
        }
    }
    Ok(())
}

async fn publish_journey_release(
    inputs: &JourneyDocument,
    project_url: &str,
    system_url: &str,
    publisher: &str,
    project: &Client,
    control: &Client,
) -> anyhow::Result<(String, Arc<ReleaseManifestWeld>)> {
    let wirings = JOURNEY_PACKAGES
        .iter()
        .flat_map(|package| {
            package.operations.iter().map(move |(wiring, _)| {
                format!("{}@{}::{wiring}=1", package.id, package.version)
                    .parse::<ReleaseWiringTarget>()
                    .map_err(anyhow::Error::msg)
            })
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
        packages: JOURNEY_PACKAGES
            .iter()
            .map(|package| PackageCoordinate::new(package.id, package.version))
            .collect::<Result<Vec<_>, _>>()?,
        wirings,
        attachments: JOURNEY_PACKAGES
            .iter()
            .map(|package| journey_publication_root(*package).join("attachments.json"))
            .collect(),
        route_host: Some(inputs.route_host.clone()),
        package_manifests: JOURNEY_PACKAGES
            .iter()
            .map(|package| journey_package_root(*package).join("wamn.json"))
            .collect(),
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
                (AuthorityClass::EventMaterializer.as_str()): credentials.event_materializer,
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
    route_host: &str,
) -> anyhow::Result<HashMap<String, String>> {
    let expected_packages = JOURNEY_PACKAGES
        .iter()
        .map(|package| PackageCoordinate::new(package.id, package.version))
        .collect::<Result<BTreeSet<_>, _>>()?;
    anyhow::ensure!(
        release.manifest().release.packages == expected_packages,
        "released manifest carries the wrong exact package membership"
    );
    let digests = release
        .manifest()
        .components
        .iter()
        .map(|component| {
            (
                component.package_id.clone(),
                component.digest.as_str().to_owned(),
            )
        })
        .collect::<HashMap<_, _>>();
    let expected = JOURNEY_PACKAGES
        .iter()
        .map(|package| package.id)
        .collect::<BTreeSet<_>>();
    let observed = digests.keys().map(String::as_str).collect::<BTreeSet<_>>();
    anyhow::ensure!(
        observed == expected,
        "released manifest carries the wrong package component closure: {observed:?}"
    );
    anyhow::ensure!(
        release.manifest().components.len() == JOURNEY_PACKAGES.len()
            && release.manifest().components.iter().all(|component| {
                JOURNEY_PACKAGES.iter().any(|package| {
                    component.package_id == package.id && component.component == package.component
                })
            }),
        "released manifest does not carry exactly one component for each package"
    );
    let overlay = release
        .manifest()
        .components
        .iter()
        .find(|component| component.package_id == OVERLAY_PACKAGE_ID)
        .context("released manifest omitted the overlay component")?;
    let dependency = ComponentOperationDependency {
        package: BASE_PACKAGE_ID.to_owned(),
        version: BASE_PACKAGE_VERSION.to_owned(),
        digest: digests
            .get(BASE_PACKAGE_ID)
            .context("released manifest omitted the base component digest")?
            .clone(),
        operation: BASE_RECORD_RECEIPT.to_owned(),
    };
    anyhow::ensure!(
        overlay
            .operations
            .get(OVERLAY_RECORD_RECEIPT)
            .is_some_and(|operation| operation.dependencies == [dependency]),
        "released overlay record_receipt omitted its exact pinned dependency fact"
    );
    let expected_attachment_ids = JOURNEY_ATTACHMENTS
        .iter()
        .map(|attachment| attachment.id)
        .collect::<BTreeSet<_>>();
    let observed_attachment_ids = release
        .manifest()
        .attachments
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        observed_attachment_ids == expected_attachment_ids,
        "released manifest carries missing or extra route attachments: {observed_attachment_ids:?}"
    );
    for expected in JOURNEY_ATTACHMENTS {
        let attachment = &release.manifest().attachments[expected.id];
        anyhow::ensure!(
            attachment.kind == AttachmentKind::Http
                && attachment.package_id == expected.package_id
                && attachment.wiring_id == expected.wiring_id
                && attachment.wiring_version == 1
                && attachment.registered_operation.as_deref() == Some(expected.operation)
                && attachment.definition["route"]["method"] == "POST"
                && attachment.definition["route"]["path"] == expected.path
                && attachment.definition["route"]["host"] == route_host
                && attachment.auth_policy["mode"] == "pat",
            "released attachment {} does not match its exact PAT route tuple: {attachment:?}",
            expected.id
        );
    }
    let registration = release
        .manifest()
        .registrations
        .get("client_acme_receiving::quality.create_inspection")
        .context("released manifest omitted the Acme receipt registration")?;
    anyhow::ensure!(
        registration.package_id == OVERLAY_PACKAGE_ID
            && registration.source_package_id == BASE_PACKAGE_ID
            && registration.entity == "receipt"
            && registration.ops == BTreeSet::from(["insert".to_owned()]),
        "released Acme receipt registration has the wrong owner/source/entity/ops: {registration:?}"
    );
    Ok(digests)
}

async fn build_journey_runtime(
    inputs: &JourneyDocument,
    credentials: &JourneyCredentials,
    release: Arc<ReleaseManifestWeld>,
) -> anyhow::Result<(
    Arc<wash_runtime::engine::Engine>,
    Component,
    Arc<FlowHttpRouting>,
    Arc<RouterDeliveryBridge>,
    tokio::task::JoinHandle<()>,
)> {
    anyhow::ensure!(
        std::fs::read_dir(&inputs.compilation_cache_directory)
            .with_context(|| {
                format!(
                    "read cold compilation cache {}",
                    inputs.compilation_cache_directory.display()
                )
            })?
            .next()
            .is_none(),
        "Receiving journey compilation cache must be empty before runtime construction"
    );
    let postgres = journey_postgres(credentials)?;
    let source = ComponentArtifactSource::new(
        ComponentArtifactSourceConfig::new(
            &inputs.component_artifact_base,
            true,
            REGISTRY_IO_TIMEOUT,
        )?
        .with_registry_auth_file(&inputs.registry_auth_file)?,
    );
    let engine = Arc::new(
        build_engine_with_host_memory_and_compilation_cache(
            &[],
            default_host_memory_budgets(),
            &inputs.compilation_cache_directory,
        )
        .context("build the cached Receiving router engine")?,
    );
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
            Arc::new(
                RouteAuthentication::new(
                    identity_reader,
                    postgres,
                    ORG,
                    PROJECT,
                    route_caller_subject(ORG, PROJECT, ENVIRONMENT)?,
                )
                .await?,
            ),
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
                '2026-08-31T12:00:00.000000Z', '2026-08-31T12:00:00.000000Z'), \
               ('00000000-0000-0000-0000-000000000302', 'PO-302', \
                '00000000-0000-0000-0000-000000000402', 'open', 1, \
                '2026-08-31T12:01:00.000000Z', '2026-08-31T12:01:00.000000Z'); \
             INSERT INTO receiving.purchase_order_line \
               (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity) \
             VALUES \
               ('00000000-0000-0000-0000-000000000501', \
                '00000000-0000-0000-0000-000000000301', 1, \
                '00000000-0000-0000-0000-000000000101', 5.0000, 0.0000), \
               ('00000000-0000-0000-0000-000000000502', \
                '00000000-0000-0000-0000-000000000302', 1, \
                '00000000-0000-0000-0000-000000000101', 7.0000, 0.0000); \
             INSERT INTO receiving.purchase_order \
               (id, purchase_order_number, supplier_id, status, row_version, created_at, updated_at, \
                acme_inspection_required, acme_quality_status) \
             VALUES \
               ('00000000-0000-0000-0000-000000000303', 'PO-303', \
                '00000000-0000-0000-0000-000000000403', 'complete', 2, \
                '2026-08-31T11:00:00.000000Z', '2026-08-31T11:30:00.000000Z', \
                true, 'pending');",
        )
        .await
        .context("seed only Receiving business rows")
}

// Distinct route-only approval precondition. This fixture does not claim that
// CDC or a materializer created the inspection; `.15.25.4` owns that proof.
async fn seed_preexisting_quality_fixture(project: &Client) -> anyhow::Result<()> {
    project
        .batch_execute(
            "INSERT INTO receiving.record_receipt_command \
               (idempotency_key, canonical_command, receipt_id, purchase_order_id, \
                purchase_order_status, row_version) \
             VALUES \
               ('quality-route-precondition', '\\x01', \
                '00000000-0000-0000-0000-000000000603', \
                '00000000-0000-0000-0000-000000000303', 'complete', 2); \
             INSERT INTO receiving.receipt \
               (id, idempotency_key, purchase_order_id, receipt_reference, occurred_at) \
             VALUES \
               ('00000000-0000-0000-0000-000000000603', 'quality-route-precondition', \
                '00000000-0000-0000-0000-000000000303', 'QUALITY-PREEXISTING', \
                '2026-08-31T11:30:00.000000Z'); \
             INSERT INTO receiving.quality_inspection (receipt_id, status, row_version) \
             VALUES ('00000000-0000-0000-0000-000000000603', 'pending', 1);",
        )
        .await
        .context("seed the distinct pre-existing quality approval fixture")
}

async fn seed_materializer_trigger_rows(project: &Client) -> anyhow::Result<()> {
    project
        .batch_execute(
            "INSERT INTO receiving.purchase_order \
               (id, purchase_order_number, supplier_id, status, row_version, created_at, updated_at) \
             VALUES \
               ('00000000-0000-0000-0000-000000000304', 'PO-304', \
                '00000000-0000-0000-0000-000000000404', 'open', 1, \
                '2026-08-31T12:03:00.000000Z', '2026-08-31T12:03:00.000000Z'); \
             INSERT INTO receiving.purchase_order_line \
               (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity) \
             VALUES \
               ('00000000-0000-0000-0000-000000000504', \
                '00000000-0000-0000-0000-000000000304', 1, \
                '00000000-0000-0000-0000-000000000101', 9.0000, 0.0000);",
        )
        .await
        .context("seed the untouched materializer journey purchase order")
}

#[tokio::test]
#[ignore = "requires disposable PG18, NATS, authenticated OCI, and built wamn/host/flow-http binaries"]
async fn product_dev_command_owns_the_clean_twelve_stage_receipt_and_cleanup() -> anyhow::Result<()>
{
    // Keep the expensive product gate to one clean run. Engine tests
    // `dirty_source_reaches_gate_then_refuses_before_publish` and
    // `dirty_watch_suffix_refuses_before_its_first_provenance_stage`, plus the
    // filesystem adapter's `filesystem_events_map_owned_inputs_and_ignore_generated_outputs`,
    // own dirty-stop and affected-suffix behavior deterministically.
    let system_url = required_journey(JOURNEY_URL_ENV)?;
    let inputs = DevJourneyInputs::required()?;
    let repository = repository_root()?;
    let source = GitSource::discover(&repository)
        .await
        .context("discover the product-command source repository")?
        .snapshot()
        .await
        .context("read the product-command source identity")?;
    anyhow::ensure!(
        source.state() == DevSourceState::Clean,
        "the live product-command proof requires a clean worktree"
    );
    let source_commit = source.source_commit().to_owned();

    let scratch = ScratchRoot::create()?;
    let root = scratch.path();
    let (admin, admin_task) = connect(&system_url).await?;
    let environment = crate::dev_environment::provision(&system_url, admin.as_ref(), root).await?;
    let publisher_subject = environment
        .route
        .management_principal_subject
        .as_deref()
        .context("project provisioning emitted no management-author principal")?;
    let publisher_id = resolve_subject(admin.as_ref(), PrincipalKind::Service, publisher_subject)
        .await
        .context("resolve the production management-author principal")?
        .context("the production management-author principal is absent")?
        .id()
        .to_string();
    let (project, project_task) = connect(&environment.route.database_url).await?;
    let (gate_bind, gate_server) = start_journey_management_gate(
        &environment.credentials,
        &environment.verification.credential_url,
        "127.0.0.1:0",
    )
    .await?;
    let system_acl_before = current_database_acl(admin.as_ref()).await?;
    let durable_acl_before = current_database_acl(project.as_ref()).await?;
    let config = write_dev_config(
        root,
        &system_url,
        &environment.route,
        &environment.credentials,
        &environment.verification,
        &gate_bind,
        &inputs.environment,
        &environment.identity,
    )?;

    let command_result = async {
        let output = run_dev_product_command(&inputs, &config).await?;
        // The literal command emits this receipt only after native workload
        // stop and supervised host reaping have both succeeded.
        verify_dev_command_receipt(&output)?;
        let system_acl_after = current_database_acl(admin.as_ref()).await?;
        let durable_acl_after = current_database_acl(project.as_ref()).await?;
        anyhow::ensure!(
            system_acl_after == system_acl_before,
            "wamn dev changed the system database ACL: \
             before={system_acl_before:?} after={system_acl_after:?}"
        );
        anyhow::ensure!(
            durable_acl_after == durable_acl_before,
            "wamn dev changed the durable database ACL: \
             before={durable_acl_before:?} after={durable_acl_after:?}"
        );
        verify_dev_target_package_and_acl_state(project.as_ref()).await?;
        verify_dev_release_state(
            admin.as_ref(),
            &inputs.environment,
            &source_commit,
            &publisher_id,
            publisher_subject,
        )
        .await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;

    gate_server.abort();
    let _ = gate_server.await;
    let verification_cleanup =
        verify_dev_verification_database_absent(admin.as_ref(), &environment.verification.database)
            .await;
    // Exact fallback cleanup runs after the assertion, so it cannot make a
    // product cleanup failure look green when an earlier stage fails.
    let fixture_database_cleanup = admin
        .batch_execute(&provision_sql::drop_database_named_sql(
            &environment.verification.database,
        ))
        .await
        .context("remove the exact verification fixture after its cleanup assertion");
    let role_cleanup =
        clean_dev_verification_gate_roles(admin.as_ref(), &environment.verification).await;
    project_task.abort();
    admin_task.abort();

    command_result?;
    verification_cleanup?;
    fixture_database_cleanup?;
    role_cleanup?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires disposable PG18 and authenticated OCI plus built virtualized base, overlay, and flow-http artifacts"]
async fn production_two_package_release_serves_all_thirteen_pat_routes() -> anyhow::Result<()> {
    let inputs = JourneyDocument::required()?;
    let system_url = inputs.system_pg_url.clone();
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
    let caller_principal_id = resolve_subject(
        admin.as_ref(),
        PrincipalKind::Service,
        &route.principal_subject,
    )
    .await
    .context("resolve the production route-caller principal")?
    .context("the production route-caller principal is absent")?
    .id()
    .to_string();
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
    verify_journey_operation_grants(project.as_ref()).await?;
    reconcile_journey_run_plane(&system_url, &route.database_url).await?;
    let credentials = prepare_journey_credentials(
        &system_url,
        &route.database_url,
        root,
        &inputs.host_secret_directory,
        &inputs.host_secret_namespace,
    )
    .await?;
    reconcile_journey_data_access(&route.database_url).await?;
    let declarations = render_component_declarations(root)?;
    push_journey_components(&inputs, &route.database_url, &system_url, &declarations).await?;
    let admitted_component_digests =
        verify_journey_components_are_effectful(project.as_ref()).await?;

    let (management_bind, management_server) = start_journey_management_gate(
        &credentials,
        &credentials.management_admitter,
        "127.0.0.1:0",
    )
    .await?;
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
    let component_digests = released_component_digests(&release, &inputs.route_host)?;
    anyhow::ensure!(
        component_digests == admitted_component_digests,
        "released component digests differ from the two admitted artifacts"
    );
    seed_receiving_business_rows(project.as_ref()).await?;

    let traces = TraceHarness::install();
    let (engine, flow_http, routing, bridge, identity_task) =
        build_journey_runtime(&inputs, &credentials, release).await?;
    let mut expected_direct_traces = Vec::new();

    let (cold_nested_trace, traceparent) = journey_trace(1);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        overlay_route_path("receiving_record_receipt"),
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"acme-record-receipt","value":{"idempotency_key":"receipt-command-2","purchase_order_id":"00000000-0000-0000-0000-000000000302","receipt_reference":"RECEIPT-2","occurred_at":"2026-08-31T12:31:00.000000Z","line":[{"purchase_order_line_id":"00000000-0000-0000-0000-000000000502","quantity":"7.0000","location_id":"00000000-0000-0000-0000-000000000201"}]}}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "acme-record-receipt")?;
    anyhow::ensure!(
        value["purchase_order_status"] == "complete"
            && value["row_version"] == "2"
            && value["acme_inspection_required"] == false
            && value["acme_quality_status"] == "not_required",
        "cold Acme receiving.record_receipt returned the wrong result: {value}"
    );

    let (_, traceparent) = journey_trace(15);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/location/list",
        Some(&route.token),
        &traceparent,
        Bytes::from_static(br#"[{"request_id":"location-list"}]"#),
    )
    .await?;
    let value = successful_value(&response, "location-list")?;
    anyhow::ensure!(
        value["rows"].as_array().is_some_and(|rows| {
            rows.len() == 1
                && rows[0]["id"] == "00000000-0000-0000-0000-000000000201"
                && rows[0]["location_code"] == "DOCK-1"
        }),
        "location.list returned the wrong rows: {value}"
    );

    let (_, traceparent) = journey_trace(16);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        "/receiving/load_receipt_screen",
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"load-receipt-screen","purchase_order_id":"00000000-0000-0000-0000-000000000301"}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "load-receipt-screen")?;
    anyhow::ensure!(
        value["rows"].as_array().is_some_and(|rows| {
            rows.len() == 1
                && rows[0]["purchase_order_id"] == "00000000-0000-0000-0000-000000000301"
                && rows[0]["line_id"] == "00000000-0000-0000-0000-000000000501"
                && rows[0]["remaining_quantity"] == "5.0000"
        }),
        "receiving.load_receipt_screen returned the wrong rows: {value}"
    );

    let (trace_id, traceparent) = journey_trace(2);
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
    expected_direct_traces.push((
        trace_id,
        "purchase_order_get",
        "wamn-receiving:purchase-order/get@1.0.0",
        BASE_PACKAGE_ID,
    ));

    let (trace_id, traceparent) = journey_trace(3);
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
    expected_direct_traces.push((
        trace_id,
        "purchase_order_query",
        "wamn-receiving:purchase-order/query@1.0.0",
        BASE_PACKAGE_ID,
    ));

    let (trace_id, traceparent) = journey_trace(4);
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
    expected_direct_traces.push((
        trace_id,
        "purchase_order_update",
        "wamn-receiving:purchase-order/update@1.0.0",
        BASE_PACKAGE_ID,
    ));

    let (trace_id, traceparent) = journey_trace(5);
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
    expected_direct_traces.push((
        trace_id,
        "receiving_record_receipt",
        BASE_RECORD_RECEIPT,
        BASE_PACKAGE_ID,
    ));

    let (trace_id, traceparent) = journey_trace(6);
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
    expected_direct_traces.push((
        trace_id,
        "receipt_get",
        "wamn-receiving:receipt/get@1.0.0",
        BASE_PACKAGE_ID,
    ));

    let (trace_id, traceparent) = journey_trace(7);
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
            items.len() == 2
                && items
                    .iter()
                    .any(|item| item["receipt_reference"] == "RECEIPT-1")
                && items
                    .iter()
                    .any(|item| item["receipt_reference"] == "RECEIPT-2")
        }),
        "receipt.query returned the wrong page: {value}"
    );
    expected_direct_traces.push((
        trace_id,
        "receipt_query",
        "wamn-receiving:receipt/query@1.0.0",
        BASE_PACKAGE_ID,
    ));
    seed_preexisting_quality_fixture(project.as_ref()).await?;

    let (trace_id, traceparent) = journey_trace(8);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        overlay_route_path("purchase_order_get"),
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"acme-purchase-order-get","id":"00000000-0000-0000-0000-000000000302"}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "acme-purchase-order-get")?;
    anyhow::ensure!(
        value["id"] == "00000000-0000-0000-0000-000000000302"
            && value["row_version"] == "2"
            && value["acme_inspection_required"] == false
            && value["acme_quality_status"] == "not_required",
        "Acme purchase_order.get returned the wrong row: {value}"
    );
    expected_direct_traces.push((
        trace_id,
        "purchase_order_get",
        "client-acme-receiving:purchase-order/get@3.0.0",
        OVERLAY_PACKAGE_ID,
    ));

    let (trace_id, traceparent) = journey_trace(9);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        overlay_route_path("purchase_order_update"),
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"acme-purchase-order-update","id":"00000000-0000-0000-0000-000000000302","expected_row_version":"2","change":{"acme_inspection_required":true,"acme_quality_status":"pending"}}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "acme-purchase-order-update")?;
    anyhow::ensure!(
        value["row_version"] == "3"
            && value["acme_inspection_required"] == true
            && value["acme_quality_status"] == "pending",
        "Acme purchase_order.update returned the wrong row: {value}"
    );
    expected_direct_traces.push((
        trace_id,
        "purchase_order_update",
        "client-acme-receiving:purchase-order/update@3.0.0",
        OVERLAY_PACKAGE_ID,
    ));

    let (trace_id, traceparent) = journey_trace(10);
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        overlay_route_path("quality_load_purchase_order_detail"),
        Some(&route.token),
        &traceparent,
        Bytes::from_static(
            br#"[{"request_id":"quality-load-detail","purchase_order_id":"00000000-0000-0000-0000-000000000302"}]"#,
        ),
    )
    .await?;
    let value = successful_value(&response, "quality-load-detail")?;
    anyhow::ensure!(
        value["id"] == "00000000-0000-0000-0000-000000000302"
            && value["row_version"] == "3"
            && value["acme_quality_status"] == "pending",
        "quality.load_purchase_order_detail returned the wrong row: {value}"
    );
    expected_direct_traces.push((
        trace_id,
        "quality_load_purchase_order_detail",
        "client-acme-receiving:quality/load-purchase-order-detail@3.0.0",
        OVERLAY_PACKAGE_ID,
    ));

    let (trace_id, traceparent) = journey_trace(11);
    let approve = serde_json::to_vec(&serde_json::json!([{
        "request_id": "quality-approve",
        "receipt_id": PREEXISTING_QUALITY_RECEIPT_ID,
        "expected_row_version": "1",
    }]))?;
    let response = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        overlay_route_path("quality_approve_inspection"),
        Some(&route.token),
        &traceparent,
        Bytes::from(approve),
    )
    .await?;
    let value = successful_value(&response, "quality-approve")?;
    anyhow::ensure!(
        value["status"] == "approved"
            && value["row_version"] == "2"
            && value["purchase_order_id"] == "00000000-0000-0000-0000-000000000303"
            && value["purchase_order_row_version"] == "3",
        "quality.approve_inspection returned the wrong result: {value}"
    );
    expected_direct_traces.push((
        trace_id,
        "quality_approve_inspection",
        "client-acme-receiving:quality/approve-inspection@3.0.0",
        OVERLAY_PACKAGE_ID,
    ));

    let removed = project
        .execute(
            "DELETE FROM app_system.permissions \
             WHERE tenant_id = $1 AND role_name = $2 AND permission = $3",
            &[&TENANT, &ROUTE_CALLER_ROLE, &BASE_RECORD_RECEIPT],
        )
        .await
        .context("remove only the pinned-base record_receipt permission")?;
    anyhow::ensure!(
        removed == 1,
        "nested-denial setup removed {removed} permission rows instead of one"
    );
    let (denied_nested_trace, denied_nested_parent) = journey_trace(12);
    let denied_nested = invoke_journey_route(
        &engine,
        &flow_http,
        Arc::clone(&routing),
        Arc::clone(&bridge),
        &inputs.route_host,
        overlay_route_path("receiving_record_receipt"),
        Some(&route.token),
        &denied_nested_parent,
        Bytes::from_static(
            br#"[{"request_id":"nested-permission-denied","value":{"idempotency_key":"receipt-command-denied","purchase_order_id":"00000000-0000-0000-0000-000000000302","receipt_reference":"RECEIPT-DENIED","occurred_at":"2026-08-31T12:32:00.000000Z","line":[{"purchase_order_line_id":"00000000-0000-0000-0000-000000000502","quantity":"1.0000","location_id":"00000000-0000-0000-0000-000000000201"}]}}]"#,
        ),
    )
    .await?;
    let denied_nested_body: Value = serde_json::from_slice(denied_nested.body())
        .context("decode the nested permission refusal")?;
    anyhow::ensure!(
        denied_nested.status() == StatusCode::FORBIDDEN
            && denied_nested_body
                == serde_json::json!({
                    "error": {
                        "code": "permission-denied",
                        "operation": BASE_RECORD_RECEIPT,
                    }
                }),
        "nested permission refusal was not the exact discoverable 403 contract: status={} body={denied_nested_body}",
        denied_nested.status()
    );

    let (unauthorized_trace, unauthorized_parent) = journey_trace(13);
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

    let (oversized_trace, oversized_parent) = journey_trace(14);
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
    for (trace_id, wiring_id, operation, package_id) in expected_direct_traces {
        let component_digest = component_digests
            .get(package_id)
            .with_context(|| format!("released {package_id} component digest is missing"))?;
        assert_direct_route_trace(
            &spans,
            &trace_id,
            wiring_id,
            operation,
            component_digest,
            &caller_principal_id,
        );
    }
    let overlay_digest = component_digests
        .get(OVERLAY_PACKAGE_ID)
        .context("released overlay component digest is missing")?;
    let base_digest = component_digests
        .get(BASE_PACKAGE_ID)
        .context("released base component digest is missing")?;
    assert_nested_record_receipt_trace(
        &spans,
        &cold_nested_trace,
        overlay_digest,
        base_digest,
        &caller_principal_id,
    );
    assert_cold_nested_acquisition(&spans, &cold_nested_trace, overlay_digest, base_digest);
    assert_nested_permission_denial_trace(
        &spans,
        &denied_nested_trace,
        overlay_digest,
        base_digest,
        &caller_principal_id,
    );
    assert_no_component_trace(&spans, &unauthorized_trace);
    assert_no_component_trace(&spans, &oversized_trace);

    // The denial arm mutates one operation grant deliberately. Its package is
    // the author of that grant, so reapply the exact coordinate before handing
    // this disposable release to the operator-managed materializer continuation.
    let base_package = JOURNEY_PACKAGES
        .into_iter()
        .find(|package| package.id == BASE_PACKAGE_ID)
        .context("find the base package in the journey release")?;
    apply_package::run(ApplyPackageArgs {
        package: journey_package_root(base_package),
        database_url: route.database_url.clone(),
        tenant: TENANT.to_owned(),
    })
    .await
    .context("restore the base package's exact operation grants")?;
    reconcile_journey_data_access(&route.database_url).await?;
    verify_journey_operation_grants(project.as_ref()).await?;
    seed_materializer_trigger_rows(project.as_ref()).await?;

    management_server.abort();
    identity_task.abort();
    project_task.abort();
    admin_task.abort();
    Ok(())
}

#[tokio::test]
#[ignore = "requires the disposable Receiving journey after its production materializer settles"]
async fn production_materializer_consumes_the_causal_receipt_exactly_once() -> anyhow::Result<()> {
    let document = JourneyDocument::required()?;
    let MaterializerPhase {
        project_pg_url: project_url,
        nats_url,
        receipt_id,
    } = document.materializer.context(
        "the journey document carries no materializer phase: the route phase must \
         provision the project environment and the trigger must produce a receipt \
         before this test runs",
    )?;
    let (project, project_task) = connect(&project_url).await?;

    let registrations = project
        .query(
            "SELECT package_id, entity_id, registration::text FROM catalog.event_registrations \
             WHERE tenant_id = $1 ORDER BY package_id COLLATE \"C\", registration_id COLLATE \"C\"",
            &[&TENANT],
        )
        .await
        .context("read the installed event-registration set")?;
    anyhow::ensure!(
        registrations.len() == 1,
        "Receiving release installed {} registrations instead of one",
        registrations.len()
    );
    let registration = &registrations[0];
    let registration_document: Value = serde_json::from_str(&registration.get::<_, String>(2))
        .context("parse the installed event registration")?;
    anyhow::ensure!(
        registration.get::<_, String>(0) == OVERLAY_PACKAGE_ID
            && registration.get::<_, String>(1) == "receipt"
            && registration_document["registration-id"] == "quality.create_inspection"
            && registration_document["package-id"] == OVERLAY_PACKAGE_ID
            && registration_document["source-package-id"] == BASE_PACKAGE_ID
            && registration_document["entity"] == "receipt"
            && registration_document["ops"] == serde_json::json!(["insert"]),
        "installed event registration is not the exact Acme receipt binding: {registration_document}"
    );

    let jetstream = async_nats::jetstream::new(
        async_nats::connect(&nats_url)
            .await
            .context("connect to the disposable event plane")?,
    );
    let mut stream = jetstream
        .get_stream(MATERIALIZER_STREAM)
        .await
        .context("read the production reader's event stream")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let (receipt_sequence, receipt_causation, inspection_causation) = loop {
        let info = stream.info().await.context("read event-stream state")?;
        let mut receipt = None;
        let mut inspection = None;
        if info.state.messages > 0 {
            for sequence in info.state.first_sequence..=info.state.last_sequence {
                let message = stream
                    .get_raw_message(sequence)
                    .await
                    .with_context(|| format!("read stored event sequence {sequence}"))?;
                let Ok(envelope) =
                    serde_json::from_slice::<wamn_event_wire::Envelope>(&message.payload)
                else {
                    continue;
                };
                if envelope.op == wamn_event_wire::Op::Insert
                    && envelope.package_id == BASE_PACKAGE_ID
                    && envelope.entity == "receipt"
                    && envelope
                        .new
                        .as_ref()
                        .and_then(|row| row.get("id"))
                        .and_then(Value::as_str)
                        == Some(receipt_id.as_str())
                {
                    receipt = envelope.causation.map(|causation| (sequence, causation));
                } else if envelope.op == wamn_event_wire::Op::Insert
                    && envelope.package_id == OVERLAY_PACKAGE_ID
                    && envelope.entity == "quality_inspection"
                    && envelope
                        .new
                        .as_ref()
                        .and_then(|row| row.get("receipt_id"))
                        .and_then(Value::as_str)
                        == Some(receipt_id.as_str())
                {
                    inspection = envelope.causation;
                }
            }
        }
        if let (Some((sequence, receipt)), Some(inspection)) = (receipt, inspection) {
            break (sequence, receipt, inspection);
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "causal receipt and inspection events did not both reach {MATERIALIZER_STREAM}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    anyhow::ensure!(
        receipt_causation.run == receipt_causation.root && receipt_causation.depth == 0,
        "route-origin receipt causation is not a root delivery: {receipt_causation:?}"
    );
    anyhow::ensure!(
        inspection_causation.run != receipt_causation.run
            && inspection_causation.root == receipt_causation.root
            && inspection_causation.depth == receipt_causation.depth + 1,
        "materializer-to-handler causation did not preserve root and advance depth: \
         receipt={receipt_causation:?} inspection={inspection_causation:?}"
    );

    let inspection_rows = project
        .query(
            "SELECT status, row_version FROM receiving.quality_inspection WHERE receipt_id = $1::text::uuid",
            &[&receipt_id],
        )
        .await
        .context("read the materialized quality inspection")?;
    anyhow::ensure!(
        inspection_rows.len() == 1
            && inspection_rows[0].get::<_, String>(0) == "pending"
            && inspection_rows[0].get::<_, i64>(1) == 1,
        "receipt {receipt_id} did not materialize to exactly one pending revision-1 inspection"
    );

    let settled_deadline = std::time::Instant::now() + Duration::from_secs(30);
    let consumer = loop {
        let consumer = stream
            .consumer_info(MATERIALIZER_DURABLE)
            .await
            .context("read the exact materializer durable")?;
        if consumer.name == MATERIALIZER_DURABLE
            && consumer.delivered.consumer_sequence == 1
            && consumer.delivered.stream_sequence == receipt_sequence
            && consumer.ack_floor.consumer_sequence == 1
            && consumer.ack_floor.stream_sequence == receipt_sequence
            && consumer.num_ack_pending == 0
            && consumer.num_pending == 0
            && consumer.num_redelivered == 0
        {
            break consumer;
        }
        anyhow::ensure!(
            std::time::Instant::now() < settled_deadline,
            "materializer durable did not settle exactly once: {consumer:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let mut dead_letters = jetstream
        .get_stream(wamn_event_wire::DEAD_LETTER_STREAM)
        .await
        .context("read the production reader's dead-letter stream")?;
    anyhow::ensure!(
        dead_letters.info().await?.state.messages == 0,
        "successful materialization emitted a dead letter"
    );

    println!(
        "RECEIVING_MATERIALIZER_PASS receipt_id={receipt_id} source_sequence={receipt_sequence} \
         consumer_sequence={} root={} depth={}",
        consumer.ack_floor.consumer_sequence, receipt_causation.root, inspection_causation.depth
    );
    drop(project);
    project_task.abort();
    Ok(())
}
