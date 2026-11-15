//! Route authority fixtures shared by host service boundary tests.

use super::PlatformRouteAuthenticator;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use wamn_catalog::{
    AdmittedComponentOperation, AttachmentTarget, AttachmentType, PackageCoordinate,
    ServingAttachment, ServingComponent, ServingManifest, ServingRelease,
};
use wamn_engine::{flow_http_routing::FlowHttpRouting, release_manifest::LoadedRelease};

/// One HTTP route, `orders`, whose policy has `modes`.
pub fn routing(modes: &Value) -> FlowHttpRouting {
    routing_policy(json!({"modes": modes}))
}

pub(super) fn routing_policy(auth_policy: Value) -> FlowHttpRouting {
    let digest = |hex: char| {
        format!("sha256:{}", hex.to_string().repeat(64))
            .parse::<wamn_digest::Digest>()
            .unwrap()
    };
    let manifest = ServingManifest::new(
        ServingRelease {
            packages: BTreeSet::from([PackageCoordinate::new(
                "cat".parse().unwrap(),
                "1.0.0".parse().unwrap(),
            )
            .unwrap()]),
        },
        BTreeSet::from([ServingComponent {
            runtime_schema: None,
            package_id: "cat".parse().unwrap(),
            descriptor: {
                let mut descriptor =
                    wamn_catalog::ComponentDescriptor::named("http-request", "0.1", digest('a'));
                descriptor.operations = BTreeMap::from([(
                    "request".into(),
                    AdmittedComponentOperation {
                        host_transaction: false,
                        input_ports: Vec::new(),
                        output_ports: Vec::new(),
                        parameters: Vec::new(),
                        committed_result_schema: None,
                        registered_operation: None,
                    },
                )]);
                descriptor
            },
        }]),
        BTreeSet::from([wamn_catalog::ServingRoute {
            package_id: "cat".parse().unwrap(),
            component: "http-request".into(),
            operation: "request".into(),
            type_: wamn_catalog::OperationType::Command,
            reads: BTreeSet::new(),
            revision: None,
            idempotency: None,
        }]),
        BTreeMap::from([(
            "orders".to_string(),
            ServingAttachment {
                type_: AttachmentType::Http,
                package_id: "cat".parse().unwrap(),
                target: AttachmentTarget::Route {
                    component: "http-request".into(),
                    operation: "request".into(),
                },
                definition_hash: digest('c'),
                definition: json!({
                    "route": {"host": "api.example.test", "path": "/orders", "method": "POST"}
                }),
                auth_policy,
                registered_operation: None,
            },
        )]),
    )
    .expect("fixture manifest is valid");
    let release = LoadedRelease::load_canonical_bytes(
        &manifest.canonical_bytes(),
        "fixture",
        wamn_engine::release_manifest::ReleaseTarget::new(
            wamn_registry::project_env_key("acme", "host-project", &"dev".parse().unwrap()),
            "dev".parse().unwrap(),
        )
        .with_applications(
            [(
                "catalog".parse().unwrap(),
                wamn_catalog::ApplicationDefinition {
                    connections: std::collections::BTreeMap::new(),
                    packages: ["cat".parse().unwrap()].into(),
                },
            )]
            .into(),
        ),
    )
    .expect("fixture manifest loads");
    FlowHttpRouting::for_test(Some(Arc::new(release))).with_authenticator(Arc::new(
        PlatformRouteAuthenticator::for_test(
            Arc::new(wamn_sql::WamnPostgres::with_provider(Arc::new(
                wamn_sql::StaticCredentialProvider::new(std::collections::HashMap::new(), None),
            ))),
            &"host-project--dev--project".parse().unwrap(),
            None,
            None,
        ),
    ))
}
