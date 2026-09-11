//! Receiving application scenarios over real platform adapters.

#[path = "../../../tests/integration/src/route_authentication_live/fresh_only.rs"]
mod fresh_only;
#[path = "../../client_acme_receiving/tests/overlay_compatibility.rs"]
mod overlay_compatibility;
#[path = "../../../tests/integration/src/route_authentication_live/p3_shell.rs"]
mod p3_shell;
#[path = "postcommit.rs"]
mod postcommit;
#[path = "../../../tests/integration/src/route_authentication_live/session_client.rs"]
mod session_client;
mod dev;
mod runtime;
mod routes;
mod sessions;
mod materializer;
mod environment;


use std::collections::{BTreeSet, HashMap};
use std::fs::Permissions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode};
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, InMemorySpanExporterBuilder, SdkTracerProvider, SpanData};
use serde_json::Value;
use tokio::process::Command;
use tokio_postgres::Client;
use tracing_subscriber::layer::SubscriberExt;
use wamn_catalog::{AttachmentKind, ComponentOperationDependency, PackageCoordinate};
use wamn_control_provision::sql as provision_sql;
use wamn_ctl::apply_package::{self, ApplyPackageArgs};
use wamn_ctl::author_wiring::{self, AuthorWiringArgs};
use wamn_ctl::dev::DevSourceState;
use wamn_ctl::dev::watch::GitSource;
use wamn_ctl::project_env_membership::{self, ProjectEnvMembershipArgs};
use wamn_ctl::publish_release::{self, PublishReleaseArgs, ReleaseWiringTarget};
use wamn_ctl::push_component::PushComponentArgs;
use wamn_ctl::push_release_manifest::{self, PushReleaseManifestArgs};
use wamn_ctl::reconcile_package_data_access::ReconcilePackageDataAccessArgs;
use wamn_gate_harness::journey::{BaseCandidate, JourneyDocument, MaterializerPhase};
use wamn_execution_host::{ROUTER_DELIVERY_ID, RouterDeliveryBridge, RouterDriver, RouterDriverConfig, WiringCacheCapacity};
use wamn_platform_identity::{PrincipalKind, create_human, issue_pat, resolve_subject, route_caller_subject};
use wamn_runtime::component_artifact_source::{ComponentArtifactSource, ComponentArtifactSourceConfig};
use wamn_runtime::engine::{build_engine_with_host_memory_and_compilation_cache, default_host_memory_budgets};
use wamn_runtime::plugins::WamnJetstream;
use wamn_runtime::plugins::flow_http_routing::{FLOW_HTTP_ROUTING_ID, FlowHttpRouting, RouteAuthentication, RouteInFlightLimit, SessionRouteAuthentication};
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_jetstream::WamnJetstreamConfig;
use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};
use wamn_runtime::plugins::wamn_postgres::{AuthorityClass, CredentialProvider, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig};
use wamn_runtime::release_manifest::ReleaseManifestWeld;
use wamn_runtime::release_manifest_source::ReleaseManifestSource;
use wamn_runtime::session_keys::{IssuerKeys, IssuerKeysConfig};
use wamn_runtime::session_verifier::SessionVerifier;
use wash_runtime::engine::InstancePolicy;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::engine::workload::{WorkloadComponent, WorkloadItem};
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::types::LocalResources;
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component, Linker};
use wasmtime_wasi_http::p3::bindings::Service;
use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode;

use wamn_ctl::dev::environment::{DevEnvironmentInputs, ENVIRONMENT, JourneyCredentials, ORG, PROJECT, RELEASE_ID, TENANT, clean_dev_verification_gate_roles, connect, install_journey_platform_floor, prepare_journey_credentials, provision_journey_control, provision_route, read_json, reconcile_journey_run_plane, secret_value, spawn_journey_management_gate, write_dev_config};
use wamn_test_infrastructure::scratch::ScratchRoot;
use runtime::{TraceHarness, journey_trace, span_attribute, span_descends_from, trace_component_invocations, assert_invocation_identity, assert_postgres_descendants, assert_direct_route_trace, assert_nested_record_receipt_trace, assert_native_nested_acquisition, assert_nested_permission_denial_trace, assert_no_component_trace, build_journey_runtime, JourneyGuestMemory, invoke_journey_route, invoke_journey_request, successful_value};
use routes::copy_fresh_only_package;
use sessions::{assert_operation_refusal, nested_receipt_state};
use materializer::{connect_event_proof_client};
use environment::{package_root, overlay_package_root, journey_package_root, required_journey, required_journey_path, journey_scenario_worker_binary, journey_publication_root, overlay_route_path, install_journey_project, reconcile_journey_data_access, verify_journey_operation_grants, repository_root, render_component_declarations, push_journey_components, verify_journey_components_are_effectful, gate_journey_wirings, verify_zero_case_gate_reports, author_journey_wirings, JourneyReleaseTarget, publish_journey_release, released_component_digests, seed_receiving_business_rows, seed_preexisting_quality_fixture, seed_materializer_trigger_rows};

const ROUTE_CALLER_ROLE: &str = "route-caller";
const BASE_PACKAGE_ID: &str = "wamn_receiving";
const BASE_PACKAGE_VERSION: &str = "1.0.0";
const BASE_COMPONENT: &str = "receiving";
const OPERATION: &str = "wamn-receiving:purchase-order/get@1.0.0";
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
const MATERIALIZER_STREAM: &str = "EVT_4_acme_9_receiving_3_dev";
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

/// The one process setting the cluster journey hands this crate: the path to
/// its input document. Everything else crosses as fields of that document.
const JOURNEY_DOCUMENT_ENV: &str = "WAMN_JOURNEY_DOCUMENT";

/// The built `wamn-scenario-worker` both live gates spawn as their Gate.
///
/// It rides as an environment variable rather than a journey-document field
/// because it is a process setting, not journey data: it names a binary this
/// machine's recipe just built, exactly like `WAMN_RECEIVING_DEV_HOST_BIN`.
const SCENARIO_WORKER_BIN_ENV: &str = "WAMN_JOURNEY_SCENARIO_WORKER_BIN";

/// Fixed nameable port for `[RECEIVING-ROUTE-JOURNEY]`'s spawned Gate.
///
/// Distinct from [`DEV_LIVE_GATE_BIND`] so that a Gate left behind by one
/// recipe fails the other loudly on bind rather than answering for it.
const ROUTE_JOURNEY_GATE_BIND: &str = "127.0.0.1:18089";
