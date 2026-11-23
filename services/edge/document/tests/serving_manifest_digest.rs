//! Digest and closed-shape tests for serving-manifest format 11.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use wamn_component_policy::{
    AdmittedComponent, AdmittedComponentOperation, ComponentPackageCoordinate,
};
use wamn_edge_document::{
    CatalogIdentityError, SERVING_MANIFEST_FORMAT_VERSION, ServingComponent, ServingManifest,
    ServingRelease, ServingRoute, route_attachments,
};
use wamn_package::canonical_name::CanonicalName;
use wamn_package::{
    AttachmentAuthPolicy, AttachmentTarget, AttachmentType, OperationType, PackageCoordinate,
    ServingAttachment, parse_attachment_auth_policy,
};

mod format_three_vector {
    include!("fixtures/release_manifest_format_three_vector.rs");
}

const COMPONENT_A: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const COMPONENT_B: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const DEFINITION: &str = "sha256:5555555555555555555555555555555555555555555555555555555555555555";

fn release() -> ServingRelease {
    ServingRelease {
        packages: BTreeSet::from([
            PackageCoordinate::new(
                "platform_fixture".parse().unwrap(),
                "1.0.0".parse().unwrap(),
            )
            .unwrap(),
            PackageCoordinate::new(
                "platform_fixture_overlay".parse().unwrap(),
                "3.0.0".parse().unwrap(),
            )
            .unwrap(),
        ]),
    }
}

/// The projection of one admitted component of `package` at `version`.
fn project(
    package: wamn_package::PackageName,
    version: &str,
    component: &str,
    interface_version: &str,
    digest: &str,
    operations: BTreeMap<CanonicalName, AdmittedComponentOperation>,
) -> ServingComponent {
    let fact = AdmittedComponent {
        data_change: false,
        target: ComponentPackageCoordinate {
            project_env_key: "org-a--dev--project".parse().unwrap(),
            package_id: package,
            package_version: version.parse().unwrap(),
        },
        component: component.parse().unwrap(),
        interface_version: interface_version.parse().unwrap(),
        operations,
        component_digest: digest.parse().unwrap(),
        imports: Vec::new(),
        imports_fingerprint: wamn_digest::canonical_json_sha256(&json!([])),
        effects: Vec::new(),
    };
    ServingComponent::project(&fact, Vec::new())
}

fn components() -> BTreeSet<ServingComponent> {
    BTreeSet::from([
        project(
            "platform_fixture".parse().unwrap(),
            "1.0.0",
            "transform",
            "0.1",
            COMPONENT_B,
            BTreeMap::from([(
                "map".parse().unwrap(),
                AdmittedComponentOperation {
                    host_transaction: false,
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    committed_result_schema: None,
                    registered_operation: None,
                },
            )]),
        ),
        project(
            "platform_fixture_overlay".parse().unwrap(),
            "3.0.0",
            "http-request",
            "0.1",
            COMPONENT_A,
            BTreeMap::from([(
                "platform-fixture-overlay:widget/get@3.0.0".parse().unwrap(),
                AdmittedComponentOperation {
                    host_transaction: false,
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    committed_result_schema: None,
                    registered_operation: Some(
                        "platform-fixture-overlay:widget/get@3.0.0".parse().unwrap(),
                    ),
                },
            )]),
        ),
    ])
}

fn routes() -> BTreeSet<ServingRoute> {
    BTreeSet::from([ServingRoute {
        package_id: "platform_fixture_overlay".parse().unwrap(),
        component: "http-request".parse().unwrap(),
        operation: "platform-fixture-overlay:widget/get@3.0.0".parse().unwrap(),
        type_: OperationType::Get,
        reads: BTreeSet::new(),
        revision: None,
        idempotency: None,
    }])
}

/// The document of these facts, admitted through its canonical bytes as a
/// serving process admits its mount.
fn admit(
    release: ServingRelease,
    components: BTreeSet<ServingComponent>,
    routes: BTreeSet<ServingRoute>,
    attachments: BTreeMap<CanonicalName, ServingAttachment>,
) -> Result<ServingManifest, CatalogIdentityError> {
    let document = ServingManifest {
        format_version: SERVING_MANIFEST_FORMAT_VERSION,
        release,
        components,
        composed_entry: None,
        routes,
        attachments: route_attachments(attachments),
        host_routes: BTreeSet::new(),
    };
    ServingManifest::from_canonical_bytes(&document.canonical_bytes()).map(|(manifest, _)| manifest)
}

fn manifest() -> ServingManifest {
    admit(
        release(),
        components(),
        routes(),
        BTreeMap::from([(
            "widget-get-http".parse().unwrap(),
            ServingAttachment {
                type_: AttachmentType::Http,
                package_id: "platform_fixture_overlay".parse().unwrap(),
                target: AttachmentTarget::Route {
                    component: "http-request".parse().unwrap(),
                    operation: "platform-fixture-overlay:widget/get@3.0.0".parse().unwrap(),
                },
                definition_hash: DEFINITION
                    .parse()
                    .expect("fixture definition hash is canonical"),
                definition: json!({
                    "id": "widget-get-http",
                    "type": "http",
                    "run-deadline-ms": 30000
                }),
                auth_policy: json!({"modes": ["pat"]}),
                registered_operation: Some(
                    "platform-fixture-overlay:widget/get@3.0.0".parse().unwrap(),
                ),
            },
        )]),
    )
    .expect("the format-eleven fixture is valid")
}

#[test]
fn the_format_three_vector_refuses_with_its_version() {
    let error = ServingManifest::from_canonical_bytes(format_three_vector::CANONICAL_BYTES)
        .expect_err("format 9 refuses the frozen format-3 bytes");
    assert!(
        matches!(
            &error,
            CatalogIdentityError::UnsupportedServingManifestVersion { requested }
                if requested == "3"
        ),
        "the format-3 bytes refuse with their version, not an unknown field: {error:?}"
    );
}

#[test]
fn authentication_modes_are_shared_and_bound_into_canonical_bytes() {
    let mut authenticated_digests = BTreeSet::new();
    for (modes, expected, allows_pat, allows_session) in [
        (json!(["none"]), AttachmentAuthPolicy::None, false, false),
        (json!(["pat"]), AttachmentAuthPolicy::Pat, true, false),
        (
            json!(["session"]),
            AttachmentAuthPolicy::Session,
            false,
            true,
        ),
        (
            json!(["pat", "session"]),
            AttachmentAuthPolicy::PatAndSession,
            true,
            true,
        ),
        (
            json!(["signature"]),
            AttachmentAuthPolicy::Signature {
                header: "X-Signature",
                credential: "webhook",
            },
            false,
            false,
        ),
    ] {
        let mut policy = json!({"modes": modes});
        if let AttachmentAuthPolicy::Signature { header, credential } = expected {
            policy["header"] = json!(header);
            policy["credential"] = json!(credential);
        }
        let parsed = parse_attachment_auth_policy(&policy).expect("supported modes");
        assert_eq!(parsed, expected);
        assert_eq!(parsed.allows_pat(), allows_pat);
        assert_eq!(parsed.allows_session(), allows_session);

        let mut candidate = manifest();
        let attachment = candidate.attachments.get_mut("widget-get-http").unwrap();
        attachment.auth_policy = policy.clone();
        if parsed.has_no_caller() {
            attachment.registered_operation = None;
        }
        let bytes = candidate.canonical_bytes();
        let (admitted, digest) = ServingManifest::from_canonical_bytes(&bytes)
            .expect("canonical policy survives the release reader");
        assert_eq!(admitted.attachments["widget-get-http"].auth_policy, policy);
        assert_eq!(admitted.canonical_bytes(), bytes);
        if parsed != AttachmentAuthPolicy::None {
            assert!(authenticated_digests.insert(digest));
        }
    }
}

#[test]
fn malformed_authentication_lists_are_refused_by_parser_and_release_reader() {
    for policy in [
        json!(null),
        json!([]),
        json!({}),
        json!({"mode": "pat"}),
        json!({"modes": null}),
        json!({"modes": "pat"}),
        json!({"modes": []}),
        json!({"modes": [null]}),
        json!({"modes": [1]}),
        json!({"modes": [""]}),
        json!({"modes": ["unknown"]}),
        json!({"modes": ["PAT"]}),
        json!({"modes": ["pat", "pat"]}),
        json!({"modes": ["session", "session"]}),
        json!({"modes": ["none", "none"]}),
        json!({"modes": ["none", "pat"]}),
        json!({"modes": ["session", "none"]}),
        json!({"modes": ["session", "pat"]}),
        json!({"modes": ["pat", "unknown"]}),
        json!({"modes": ["pat", "session", "session"]}),
        json!({"modes": ["pat"], "mode": "pat"}),
        json!({"modes": ["pat"], "unknown": true}),
    ] {
        assert_eq!(parse_attachment_auth_policy(&policy), None);
        let mut candidate = manifest();
        candidate
            .attachments
            .get_mut("widget-get-http")
            .unwrap()
            .auth_policy = policy;
        assert_eq!(
            ServingManifest::from_canonical_bytes(&candidate.canonical_bytes()),
            Err(CatalogIdentityError::InvalidAttachmentAuthPolicy {
                attachment_id: "widget-get-http".parse().unwrap(),
            })
        );
    }
}

/// A provider and a consumer, where `package` owns the component that exports
/// `export`.
///
/// The owning package id is a parameter rather than the export's namespace,
/// because production does not tie the two together: a component that exports
/// the platform interface `wamn:node/handler@0.2.0` carries the application's own
/// package id, as the fixture of `crates/publish/src/publish_release.rs` shows
/// with `http-request` under `"package-id":"orders"`. Deriving the id from the
/// namespace produced the reserved id `wamn`, which is not a manifest any
/// publisher can publish.
fn operation_provider_manifest(
    package: &wamn_package::PackageName,
    export: &str,
    version: &str,
) -> ServingManifest {
    let operation = AdmittedComponentOperation {
        host_transaction: false,
        input_ports: Vec::new(),
        output_ports: Vec::new(),
        parameters: Vec::new(),
        registered_operation: None,
        committed_result_schema: None,
    };
    admit(
        ServingRelease {
            packages: BTreeSet::from([
                PackageCoordinate::new(package.clone(), version.parse().unwrap()).unwrap(),
                PackageCoordinate::new("consumer".parse().unwrap(), "1.0.0".parse().unwrap())
                    .unwrap(),
            ]),
        },
        BTreeSet::from([
            project(
                package.clone(),
                version,
                "provider",
                "0.1.0",
                COMPONENT_A,
                BTreeMap::from([(
                    export.parse().unwrap(),
                    AdmittedComponentOperation {
                        // Registered only when `package` owns the export's
                        // namespace, which is the rule
                        // `validate_canonical_operation_for_package` enforces. A
                        // platform interface is exported as a palette operation,
                        // and a palette operation carries no registered
                        // operation.
                        registered_operation: export
                            .starts_with(&format!("{package}:"))
                            .then(|| export.parse().unwrap()),

                        ..operation.clone()
                    },
                )]),
            ),
            project(
                "consumer".parse().unwrap(),
                "1.0.0",
                "consumer",
                "0.1.0",
                COMPONENT_B,
                BTreeMap::from([("consumer:entry/run@1.0.0".parse().unwrap(), operation)]),
            ),
        ]),
        BTreeSet::new(),
        BTreeMap::new(),
    )
    .expect("one provider and one consumer form a valid manifest")
}

#[test]
fn duplicate_export_only_interfaces_are_admitted() {
    // The platform interface is exported by an application component under the
    // application's own package id, the shape a published manifest carries.
    for (package, export, version) in [
        ("orders", "wamn:node/handler@0.2.0", "0.1.0"),
        ("provider", "provider:entry/run@1.0.0", "1.0.0"),
    ] {
        let mut candidate = operation_provider_manifest(&package.parse().unwrap(), export, version);
        let mut other = candidate
            .components
            .iter()
            .find(|component| component.descriptor.component == "provider")
            .expect("fixture provider")
            .clone();
        other.descriptor.component = "another-provider".parse().unwrap();
        other.descriptor.component_digest = COMPONENT_B.parse().unwrap();
        let operation = other.descriptor.operations.get_mut(export).unwrap();
        operation.registered_operation = None;
        assert!(candidate.components.insert(other));
        let (admitted, _) = ServingManifest::from_canonical_bytes(&candidate.canonical_bytes())
            .expect("the host can address duplicate export-only interfaces directly");
        assert_eq!(admitted, candidate);
    }
}
