//! Receiving application scenarios over real platform adapters.

#[cfg(feature = "cluster")]
mod cluster;
#[path = "receiving_command_histories_live.rs"]
mod command_histories;
mod fresh_only;
use command_histories::database::{FIXTURE_PRINCIPAL, bind_fixture_principal};
use wamn_integration_tests::startup_burst;
mod delivery;
mod dev;
mod environment;
mod local_business;
mod materializer;
#[path = "../../client_acme_receiving/tests/overlay_compatibility.rs"]
mod overlay_compatibility;
#[path = "postcommit.rs"]
mod postcommit;
mod routes;
mod runtime;
mod session_client;
mod sessions;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::Permissions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode};
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{
    InMemorySpanExporter, InMemorySpanExporterBuilder, SdkTracerProvider, SpanData,
};
use serde_json::Value;
use tokio::process::Command;
use tokio_postgres::Client;
use tracing_subscriber::layer::SubscriberExt;
use wamn_catalog::{AttachmentType, PackageCoordinate};
use wamn_control::apply_package::{self, ApplyPackageRequest};
use wamn_control::author_wiring::{self, AuthorWiringRequest};
use wamn_control::dev::watch::GitSource;
use wamn_control::git_source::GitSourceState;
use wamn_control::project_env_membership::{self, ProjectEnvMembershipRequest};
use wamn_control::publish_release::{self, PublishReleaseRequest, ReleaseWiringTarget};
use wamn_control::push_component::{AdmitComponentRequest, PublishAdmittedComponentRequest};
use wamn_control::push_release_manifest::{self, PushReleaseManifestRequest};
use wamn_control::reconcile_package_data_access::ReconcilePackageDataAccessRequest;
use wamn_engine::engine::{
    build_engine_with_host_memory_and_compilation_cache, default_host_memory_budgets,
};
use wamn_engine::flow_http_routing::{FlowHttpRouting, RouteInFlightLimit};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_execution_host::{OperationHost, OperationScope, RouterDeliveryBridge};
use wamn_gate_harness::journey::{BaseCandidate, JourneyDocument, MaterializerPhase};
use wamn_platform_identity::{
    PrincipalType, create_user, issue_pat, operator_subject, resolve_subject,
};
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_runtime::plugins::WamnJetstream;
use wamn_runtime::plugins::route_authentication::{
    PlatformRouteAuthenticator, RouteAuthentication, SessionRouteAuthentication,
};
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_jetstream::WamnJetstreamConfig;
use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, CredentialProvider, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};
use wamn_runtime::release_manifest_source::ReleaseManifestSource;
use wamn_runtime::session_keys::{IssuerKeys, IssuerKeysConfig};
use wamn_session::verifier::SessionVerifier;
use wamn_workflow::{RouterDriver, RouterDriverConfig, WiringCacheCapacity};
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::wasmtime::component::Component;
use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode;

use environment::{
    JourneyReleaseTarget, author_journey_wirings, gate_journey_wirings, install_journey_project,
    journey_package_root, journey_publication_root, journey_scenario_worker_binary,
    overlay_package_root, overlay_route_path, package_root, publish_and_push_journey_release,
    push_journey_components, reconcile_journey_data_access, reference_of,
    released_component_digests, render_component_declarations, repository_root, required_journey,
    required_journey_path, seed_materializer_order, seed_materializer_trigger_rows,
    seed_preexisting_quality_fixture, seed_receiving_business_rows,
    verify_journey_components_are_effectful, verify_journey_operation_grants,
    verify_zero_case_gate_reports, with_generated_attachments,
};
use routes::copy_fresh_only_package;
use runtime::{
    JourneyGuestMemory, JourneyRuntime, TraceHarness, assert_direct_route_trace,
    assert_invocation_identity, assert_native_nested_acquisition,
    assert_nested_permission_denial_trace, assert_nested_record_receipt_trace,
    assert_no_component_trace, assert_postgres_descendants, build_journey_runtime,
    invoke_journey_method, invoke_journey_read, invoke_journey_route, journey_trace,
    span_attribute, span_descends_from, successful_read_value, successful_value,
    trace_component_invocations,
};
use sessions::{assert_operation_refusal, nested_receipt_state};
use wamn_control::delivery::{Candidate, ReleaseIdentity};
use wamn_control::dev::environment::{
    DevEnvironmentInputs, ENVIRONMENT, JourneyCredentials, JourneyScope, ORG, PROJECT, RELEASE_ID,
    TENANT, connect, install_journey_platform_floor, prepare_journey_credentials,
    provision_journey_control, provision_route, reconcile_journey_run_plane,
    spawn_journey_management_gate, write_dev_config,
};
use wamn_control::provision_project_env::{read_json, secret_value};
use wamn_test_infrastructure::operations::{package_version, sealed};
use wamn_test_infrastructure::scratch::ScratchRoot;

const ADMIN_ROLE: &str = "admin";
const BASE_PACKAGE_ID: &str = "wamn_receiving";
/// The version of the base package, from its wamn.json.
static BASE_PACKAGE_VERSION: LazyLock<&'static str> =
    LazyLock::new(|| package_version(BASE_PACKAGE_ID).leak());
const BASE_COMPONENT: &str = "receiving";
// Each operation is named by reference, as the authored files name it, and
// `sealed` reads its version from the package wamn.json.
static OPERATION: LazyLock<&'static str> =
    LazyLock::new(|| sealed("wamn-receiving:purchase-order/get").leak());
const OVERLAY_PACKAGE_ID: &str = "client_acme_receiving";
/// The version of the overlay package, from its wamn.json.
static OVERLAY_PACKAGE_VERSION: LazyLock<&'static str> =
    LazyLock::new(|| package_version(OVERLAY_PACKAGE_ID).leak());
const OVERLAY_COMPONENT: &str = "client_acme_receiving";
const RAW_BODY_LIMIT: usize = 1024 * 1024;
/// Domain of the platform principal emails in every test tenant.
const PLATFORM_DOMAIN: &str = "example.invalid";
const REGISTRY_IO_TIMEOUT: Duration = Duration::from_secs(30);
// Eleven base operations: the supplier record (wamn-rm14.8) added
// supplier_create and supplier_query to the nine before it.
const BASE_OPERATIONS: [(&str, &str); 11] = [
    ("location_list", "wamn-receiving:location/list"),
    ("purchase_order_get", "wamn-receiving:purchase-order/get"),
    (
        "purchase_order_query",
        "wamn-receiving:purchase-order/query",
    ),
    (
        "purchase_order_update",
        "wamn-receiving:purchase-order/update",
    ),
    ("receipt_get", "wamn-receiving:receipt/get"),
    ("receipt_query", "wamn-receiving:receipt/query"),
    (
        "receiving_load_purchase_order_history",
        "wamn-receiving:receiving/load-purchase-order-history",
    ),
    (
        "receiving_load_receipt_screen",
        "wamn-receiving:receiving/load-receipt-screen",
    ),
    (
        "receiving_record_receipt",
        "wamn-receiving:receiving/record-receipt",
    ),
    ("supplier_create", "wamn-receiving:supplier/create"),
    ("supplier_query", "wamn-receiving:supplier/query"),
];
const OVERLAY_OPERATIONS: [(&str, &str); 6] = [
    (
        "purchase_order_get",
        "client-acme-receiving:purchase-order/get",
    ),
    (
        "purchase_order_update",
        "client-acme-receiving:purchase-order/update",
    ),
    (
        "receiving_record_receipt",
        "client-acme-receiving:receiving/record-receipt",
    ),
    (
        "quality_load_purchase_order_detail",
        "client-acme-receiving:quality/load-purchase-order-detail",
    ),
    (
        "quality_approve_inspection",
        "client-acme-receiving:quality/approve-inspection",
    ),
    (
        "quality_create_inspection",
        "client-acme-receiving:quality/create-inspection",
    ),
];
/// The reference that the authored base declaration names the operation by.
const BASE_RECORD_RECEIPT_REFERENCE: &str = "wamn-receiving:receiving/record-receipt";
static BASE_RECORD_RECEIPT: LazyLock<&'static str> =
    LazyLock::new(|| sealed(BASE_RECORD_RECEIPT_REFERENCE).leak());
static HISTORY_OPERATION: LazyLock<&'static str> =
    LazyLock::new(|| sealed("wamn-receiving:receiving/load-purchase-order-history").leak());
static OVERLAY_RECORD_RECEIPT: LazyLock<&'static str> =
    LazyLock::new(|| sealed("client-acme-receiving:receiving/record-receipt").leak());
static OVERLAY_RECEIPT_PARTICIPANT: LazyLock<&'static str> =
    LazyLock::new(|| sealed("client-acme-receiving:receiving/record-receipt-participant").leak());
const PREEXISTING_QUALITY_RECEIPT_ID: &str = "00000000-0000-0000-0000-000000000603";

/// The release this fixture provisions and publishes. A supplied candidate names
/// it, and with no candidate it is the default Receiving journey release.
fn identity() -> &'static ReleaseIdentity {
    static IDENTITY: LazyLock<ReleaseIdentity> = LazyLock::new(|| {
        let candidate = Candidate::from_env().expect("read the supplied Receiving candidate");
        match candidate {
            Some(candidate) => candidate
                .identity()
                .expect("read the release identity of the supplied Receiving candidate"),
            None => ReleaseIdentity {
                org: ORG.to_owned(),
                project: PROJECT.to_owned(),
                environment: ENVIRONMENT.to_owned(),
                tenant: TENANT.to_owned(),
                effective_release_id: RELEASE_ID,
                route_host: "receiving.localhost".to_owned(),
                packages: JOURNEY_PACKAGES
                    .iter()
                    .map(|package| {
                        wamn_catalog::PackageCoordinate::new(package.id, package.version())
                    })
                    .collect::<Result<_, _>>()
                    .expect("read the Receiving journey package coordinates"),
            },
        }
    });
    &IDENTITY
}

/// The journey packages that the release of [`identity`] carries.
fn released_journey_packages() -> impl Iterator<Item = JourneyPackage> {
    JOURNEY_PACKAGES.into_iter().filter(|package| {
        identity()
            .packages
            .iter()
            .any(|coordinate| coordinate.package_id() == package.id)
    })
}

/// The root of each released journey package, by component.
fn released_package_roots() -> BTreeMap<&'static str, PathBuf> {
    released_journey_packages()
        .map(|package| (package.component, journey_package_root(package, None)))
        .collect()
}

/// The scope of [`identity`], as the shared provisioning steps take it.
fn scope() -> JourneyScope {
    identity().into()
}

/// The event stream of the journey environment.
fn materializer_stream() -> &'static str {
    static STREAM: LazyLock<String> = LazyLock::new(|| {
        wamn_control_provision::event_stream_name(
            &identity().org,
            &identity().project,
            &identity().environment,
        )
    });
    &STREAM
}

/// The durable consumer of the Acme inspection handler.
fn materializer_durable() -> &'static str {
    static DURABLE: LazyLock<String> = LazyLock::new(|| {
        format!(
            "mat_{}_client_acme_receiving_quality_create_inspection",
            identity().tenant
        )
    });
    &DURABLE
}

#[derive(Clone, Copy)]
struct JourneyAttachment {
    id: &'static str,
    package_id: &'static str,
    wiring_id: &'static str,
    path: &'static str,
    /// Publish derives it from the operation kind: GET for a read, else POST.
    method: &'static str,
    operation: &'static str,
}

// Deployment-owned route spellings live in this one publication table rather
// than leaking into operation or component identity. The base publishes 11
// routes: the supplier record (wamn-rm14.8) added supplier-create-http and
// supplier-query-http. The overlay publishes 5.
const JOURNEY_ATTACHMENTS: [JourneyAttachment; 16] = [
    JourneyAttachment {
        id: "location-list-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "location_list",
        path: "/location/list",
        method: "GET",
        operation: "wamn-receiving:location/list",
    },
    JourneyAttachment {
        id: "purchase-order-get-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "purchase_order_get",
        path: "/purchase_order/get",
        method: "GET",
        operation: "wamn-receiving:purchase-order/get",
    },
    JourneyAttachment {
        id: "purchase-order-query-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "purchase_order_query",
        path: "/purchase_order/query",
        method: "GET",
        operation: "wamn-receiving:purchase-order/query",
    },
    JourneyAttachment {
        id: "purchase-order-update-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "purchase_order_update",
        path: "/purchase_order/update",
        method: "POST",
        operation: "wamn-receiving:purchase-order/update",
    },
    JourneyAttachment {
        id: "receipt-get-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "receipt_get",
        path: "/receipt/get",
        method: "GET",
        operation: "wamn-receiving:receipt/get",
    },
    JourneyAttachment {
        id: "receipt-query-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "receipt_query",
        path: "/receipt/query",
        method: "GET",
        operation: "wamn-receiving:receipt/query",
    },
    JourneyAttachment {
        id: "receiving-record-receipt-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "receiving_record_receipt",
        path: "/receiving/record_receipt",
        method: "POST",
        operation: BASE_RECORD_RECEIPT_REFERENCE,
    },
    JourneyAttachment {
        id: "receiving-load-receipt-screen-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "receiving_load_receipt_screen",
        path: "/receiving/load_receipt_screen",
        method: "GET",
        operation: "wamn-receiving:receiving/load-receipt-screen",
    },
    JourneyAttachment {
        id: "receiving-load-purchase-order-history-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "receiving_load_purchase_order_history",
        path: "/receiving/load_purchase_order_history",
        method: "GET",
        operation: "wamn-receiving:receiving/load-purchase-order-history",
    },
    JourneyAttachment {
        id: "supplier-create-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "supplier_create",
        path: "/supplier/create",
        method: "POST",
        operation: "wamn-receiving:supplier/create",
    },
    JourneyAttachment {
        id: "supplier-query-http",
        package_id: BASE_PACKAGE_ID,
        wiring_id: "supplier_query",
        path: "/supplier/query",
        method: "GET",
        operation: "wamn-receiving:supplier/query",
    },
    JourneyAttachment {
        id: "client-acme-receiving-purchase-order-get-http",
        package_id: OVERLAY_PACKAGE_ID,
        wiring_id: "purchase_order_get",
        path: "/acme/purchase_order/get",
        method: "GET",
        operation: "client-acme-receiving:purchase-order/get",
    },
    JourneyAttachment {
        id: "client-acme-receiving-purchase-order-update-http",
        package_id: OVERLAY_PACKAGE_ID,
        wiring_id: "purchase_order_update",
        path: "/acme/purchase_order/update",
        method: "POST",
        operation: "client-acme-receiving:purchase-order/update",
    },
    JourneyAttachment {
        id: "client-acme-receiving-receiving-record-receipt-http",
        package_id: OVERLAY_PACKAGE_ID,
        wiring_id: "receiving_record_receipt",
        path: "/acme/receiving/record_receipt",
        method: "POST",
        operation: "client-acme-receiving:receiving/record-receipt",
    },
    JourneyAttachment {
        id: "client-acme-receiving-quality-load-purchase-order-detail-http",
        package_id: OVERLAY_PACKAGE_ID,
        wiring_id: "quality_load_purchase_order_detail",
        path: "/acme/quality/load_purchase_order_detail",
        method: "GET",
        operation: "client-acme-receiving:quality/load-purchase-order-detail",
    },
    JourneyAttachment {
        id: "client-acme-receiving-quality-approve-inspection-http",
        package_id: OVERLAY_PACKAGE_ID,
        wiring_id: "quality_approve_inspection",
        path: "/acme/quality/approve_inspection",
        method: "POST",
        operation: "client-acme-receiving:quality/approve-inspection",
    },
];

#[derive(Clone, Copy)]
struct JourneyPackage {
    id: &'static str,
    component: &'static str,
    operations: &'static [(&'static str, &'static str)],
    /// The wirings the package publishes. Every other operation is a route.
    wirings: &'static [&'static str],
}

impl JourneyPackage {
    /// The version of the package, from its wamn.json.
    fn version(&self) -> &'static str {
        if self.id == BASE_PACKAGE_ID {
            *BASE_PACKAGE_VERSION
        } else {
            *OVERLAY_PACKAGE_VERSION
        }
    }
}

const JOURNEY_PACKAGES: [JourneyPackage; 2] = [
    JourneyPackage {
        id: BASE_PACKAGE_ID,
        component: BASE_COMPONENT,
        operations: &BASE_OPERATIONS,
        wirings: &[],
    },
    JourneyPackage {
        id: OVERLAY_PACKAGE_ID,
        component: OVERLAY_COMPONENT,
        operations: &OVERLAY_OPERATIONS,
        // The receipt-insert registration names this event handler wiring.
        wirings: &["quality_create_inspection"],
    },
];

/// The one process setting the cluster journey hands this crate: the path to
/// its input document. Everything else crosses as fields of that document.
const JOURNEY_DOCUMENT_ENV: &str = "WAMN_JOURNEY_DOCUMENT";

/// The built `wamn-scenario-worker` the route journey spawns as its Gate.
///
/// It rides as an environment variable rather than a journey-document field
/// because it is a process setting, not journey data: it names a binary this
/// machine's recipe just built, exactly like `WAMN_RECEIVING_DEV_HOST_BIN`.
const SCENARIO_WORKER_BIN_ENV: &str = "WAMN_JOURNEY_SCENARIO_WORKER_BIN";

/// Fixed nameable port for `[RECEIVING-ROUTE-JOURNEY]`'s spawned Gate.
const ROUTE_JOURNEY_GATE_BIND: &str = "127.0.0.1:18089";
