//! Receiving application scenarios over real platform adapters.

#[cfg(feature = "cluster")]
mod cluster;
#[path = "receiving_command_histories_live.rs"]
mod command_histories;
use command_histories::database::bind_fixture_principal;

mod delivery;
mod dev;
mod environment;
mod local_business;
mod materializer;
#[path = "../../client_acme_receiving/tests/overlay_compatibility.rs"]
mod overlay_compatibility;
mod package_copies;
#[path = "postcommit.rs"]
mod postcommit;
mod queue_recovery;
mod routes;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::Context;
use serde_json::Value;
use tokio::process::Command;
use tokio_postgres::Client;
use wamn_catalog::{AdmittedComponent, PackageCoordinate};
use wamn_control::apply_package::{self, ApplyPackageRequest};
use wamn_control::author_wiring;
use wamn_control::component_admission::{AdmitComponentRequest, admit_component};
use wamn_control::dev::watch::GitSource;
use wamn_control::git_source::GitSourceState;
use wamn_control::publish_release::{self, PublishReleaseRequest, ReleaseWiringTarget};
use wamn_control::reconcile_package_data_access::ReconcilePackageDataAccessRequest;
use wamn_gate_harness::journey::{JourneyDocument, MaterializerPhase};
use wamn_runtime::release_manifest_source::ReleaseManifestSource;

use environment::{
    JourneyReleaseTarget, admit_journey_components, author_journey_wirings, gate_journey_wirings,
    install_journey_project, journey_package_root, journey_publication_root, overlay_package_root,
    overlay_route_path, package_root, reconcile_journey_data_access, render_component_declarations,
    repository_root, required_journey, required_journey_path,
    verify_journey_components_are_effectful, verify_journey_operation_grants,
    verify_zero_case_gate_reports,
};
use routes::copy_package;
use wamn_control::delivery::{Candidate, ReleaseIdentity};
use wamn_control::dev::environment::{
    DevEnvironmentInputs, ENVIRONMENT, JourneyScope, ORG, PROJECT, TENANT, connect,
    install_journey_platform_floor, prepare_journey_credentials, provision_journey_control,
    provision_route, reconcile_journey_run_plane, spawn_journey_management_gate, write_dev_config,
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
const OVERLAY_PACKAGE_ID: &str = "client_acme_receiving";
/// The version of the overlay package, from its wamn.json.
static OVERLAY_PACKAGE_VERSION: LazyLock<&'static str> =
    LazyLock::new(|| package_version(OVERLAY_PACKAGE_ID).leak());
const OVERLAY_COMPONENT: &str = "client_acme_receiving";
/// Domain of the platform principal emails in every test tenant.
const PLATFORM_DOMAIN: &str = "example.invalid";
// Twelve base operations: the supplier record (wamn-rm14.8) added
// supplier_create and supplier_query to the nine before it, and numbering
// (wamn-83eth.3) added purchase_order_create.
const BASE_OPERATIONS: [(&str, &str); 12] = [
    ("location_list", "wamn-receiving:location/list"),
    (
        "purchase_order_create",
        "wamn-receiving:purchase-order/create",
    ),
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
        "client-acme-receiving:acme/record-receipt",
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

/// The release this fixture provisions and publishes: the default Receiving
/// journey release, with the packages of a supplied candidate's manifest.
fn identity() -> &'static ReleaseIdentity {
    static IDENTITY: LazyLock<ReleaseIdentity> = LazyLock::new(|| {
        // A cluster case takes its packages from its candidate; the other
        // Receiving tests run this checkout's packages.
        let candidate = std::env::var_os(wamn_control::delivery::CANDIDATE_ENV)
            .map(|_| Candidate::from_env().expect("read the supplied Receiving candidate"));
        ReleaseIdentity {
            org: ORG.to_owned(),
            project: PROJECT.to_owned(),
            environment: ENVIRONMENT.to_owned(),
            tenant: TENANT.to_owned(),
            route_host: "receiving.localhost".to_owned(),
            packages: match candidate {
                Some(candidate) => {
                    candidate
                        .manifest()
                        .expect("read the manifest of the supplied Receiving candidate")
                        .0
                        .release
                        .packages
                }
                None => JOURNEY_PACKAGES
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
        .map(|package| (package.component, journey_package_root(package)))
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
    package_id: &'static str,
    wiring_id: &'static str,
    path: &'static str,
}

// Deployment-owned route spellings live in this one publication table rather
// than leaking into operation or component identity. The base publishes 11
// routes: the supplier record (wamn-rm14.8) added supplier-create-http and
// supplier-query-http. The overlay publishes 5.
const JOURNEY_ATTACHMENTS: [JourneyAttachment; 16] = [
    JourneyAttachment {
        package_id: BASE_PACKAGE_ID,
        wiring_id: "location_list",
        path: "/location/list",
    },
    JourneyAttachment {
        package_id: BASE_PACKAGE_ID,
        wiring_id: "purchase_order_get",
        path: "/purchase_order/get",
    },
    JourneyAttachment {
        package_id: BASE_PACKAGE_ID,
        wiring_id: "purchase_order_query",
        path: "/purchase_order/query",
    },
    JourneyAttachment {
        package_id: BASE_PACKAGE_ID,
        wiring_id: "purchase_order_update",
        path: "/purchase_order/update",
    },
    JourneyAttachment {
        package_id: BASE_PACKAGE_ID,
        wiring_id: "receipt_get",
        path: "/receipt/get",
    },
    JourneyAttachment {
        package_id: BASE_PACKAGE_ID,
        wiring_id: "receipt_query",
        path: "/receipt/query",
    },
    JourneyAttachment {
        package_id: BASE_PACKAGE_ID,
        wiring_id: "receiving_record_receipt",
        path: "/receiving/record_receipt",
    },
    JourneyAttachment {
        package_id: BASE_PACKAGE_ID,
        wiring_id: "receiving_load_receipt_screen",
        path: "/receiving/load_receipt_screen",
    },
    JourneyAttachment {
        package_id: BASE_PACKAGE_ID,
        wiring_id: "receiving_load_purchase_order_history",
        path: "/receiving/load_purchase_order_history",
    },
    JourneyAttachment {
        package_id: BASE_PACKAGE_ID,
        wiring_id: "supplier_create",
        path: "/supplier/create",
    },
    JourneyAttachment {
        package_id: BASE_PACKAGE_ID,
        wiring_id: "supplier_query",
        path: "/supplier/query",
    },
    JourneyAttachment {
        package_id: OVERLAY_PACKAGE_ID,
        wiring_id: "purchase_order_get",
        path: "/acme/purchase_order/get",
    },
    JourneyAttachment {
        package_id: OVERLAY_PACKAGE_ID,
        wiring_id: "purchase_order_update",
        path: "/acme/purchase_order/update",
    },
    JourneyAttachment {
        package_id: OVERLAY_PACKAGE_ID,
        wiring_id: "receiving_record_receipt",
        path: "/acme/acme/record_receipt",
    },
    JourneyAttachment {
        package_id: OVERLAY_PACKAGE_ID,
        wiring_id: "quality_load_purchase_order_detail",
        path: "/acme/quality/load_purchase_order_detail",
    },
    JourneyAttachment {
        package_id: OVERLAY_PACKAGE_ID,
        wiring_id: "quality_approve_inspection",
        path: "/acme/quality/approve_inspection",
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

/// Fixed nameable port for `[RECEIVING-ROUTE-JOURNEY]`'s spawned Gate.
const ROUTE_JOURNEY_GATE_BIND: &str = "127.0.0.1:18089";
