//! Route authority fixtures shared by capability boundary tests.

use super::PlatformRouteAuthenticator;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use wamn_catalog::{
    AdmittedComponentOperation, AttachmentTarget, AttachmentType, DefinitionHash,
    PackageCoordinate, ServingAttachment, ServingComponent, ServingManifest, ServingRelease,
};
use wamn_engine::{flow_http_routing::FlowHttpRouting, release_manifest::LoadedRelease};

/// One HTTP route, `orders`, whose policy has `modes`.
pub fn routing(modes: &Value) -> FlowHttpRouting {
    routing_policy(json!({"modes": modes}))
}

pub(super) fn routing_policy(auth_policy: Value) -> FlowHttpRouting {
    let digest = |hex: char| format!("sha256:{}", hex.to_string().repeat(64));
    let manifest = ServingManifest::new(
        ServingRelease {
            packages: BTreeSet::from([PackageCoordinate::new("cat", "1.0.0").unwrap()]),
        },
        BTreeSet::from([ServingComponent {
            runtime_schema: None,
            statement_instances: std::collections::BTreeMap::new(),
            package_id: "cat".into(),
            descriptor: {
                let mut descriptor =
                    wamn_catalog::ComponentDescriptor::named("http-request", "0.1", digest('a'));
                descriptor.operations = BTreeMap::from([(
                    "request".into(),
                    AdmittedComponentOperation {
                        connections: Vec::new(),
                        host_transaction: false,
                        input_ports: Vec::new(),
                        output_ports: Vec::new(),
                        parameters: Vec::new(),
                        committed_result_schema: None,
                        registered_operation: None,
                        statements: BTreeMap::new(),
                    },
                )]);
                descriptor
            },
        }]),
        BTreeSet::from([wamn_catalog::ServingRoute {
            package_id: "cat".into(),
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
                package_id: "cat".into(),
                target: AttachmentTarget::Route {
                    component: "http-request".into(),
                    operation: "request".into(),
                },
                definition_hash: DefinitionHash::parse(digest('c')).unwrap(),
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
            wamn_registry::project_env_key("acme", "host-project", "dev"),
            "dev",
        )
        .with_applications(
            [(
                "catalog".to_owned(),
                wamn_catalog::ApplicationDefinition {
                    connections: std::collections::BTreeMap::new(),
                    packages: ["cat".to_owned()].into(),
                },
            )]
            .into(),
        ),
    )
    .expect("fixture manifest loads");
    FlowHttpRouting::for_test(Some(Arc::new(release))).with_authenticator(Arc::new(
        PlatformRouteAuthenticator::for_test(
            Arc::new(wamn_postgres_capability::WamnPostgres::with_provider(
                Arc::new(wamn_postgres_capability::StaticCredentialProvider::new(
                    std::collections::HashMap::new(),
                    None,
                )),
            )),
            "host-project",
            None,
            None,
        ),
    ))
}
