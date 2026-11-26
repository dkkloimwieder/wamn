//! Two-package effective release from the prepared fixture components.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use wamn_package::ComponentDeclaration;
use wamn_package::OperationType;
use wamn_package::PackageCoordinate;
use wamn_package::package_version::PackageVersion;

use wamn_component_policy::{ComponentAdmissionRequest, ComponentPackageCoordinate};
use wamn_digest::bytes_sha256;
use wamn_engine::component_admission::validate_component_admission;

use wamn_release::publish_release::{
    PublishReleaseManifest, ReleaseComponents, publish_release_with_package_manifests,
    resolve_route_methods,
};
use wamn_release::release_source::{read_package_attachments, read_package_manifests};

const PROJECT_ENV_KEY: &str = "effective-release-poc--dev--fixture";
const ENV: &str = "dev";
const BASE_WASM_ENV: &str = "WAMN_EFFECTIVE_RELEASE_BASE_COMPONENT_WASM";
const OVERLAY_WASM_ENV: &str = "WAMN_EFFECTIVE_RELEASE_OVERLAY_COMPONENT_WASM";
/// The fixture version, read from the compiled manifest that authors it. The
/// base and the overlay move together, so the overlay carries the same one.
fn package_version() -> &'static PackageVersion {
    static VERSION: std::sync::LazyLock<PackageVersion> = std::sync::LazyLock::new(|| {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&wamn_fixture_package::manifest_bytes())
                .expect("the compiled fixture manifest is JSON");
        serde_json::from_value(manifest["package"]["version"].take())
            .expect("the compiled fixture manifest declares a canonical version")
    });
    &VERSION
}

struct PackageInput {
    id: &'static str,
    version: &'static PackageVersion,
    root: PathBuf,
    component: &'static str,
    component_bytes: PathBuf,
    expected_component_digest: Option<wamn_digest::Digest>,
}

/// The base component digest, read from the ONE file that authors it.
///
/// wamn-10yt.50: this test used to restate the same `sha256:` literal the
/// overlay manifest pins, which is a third copy of a value that must be one.
fn base_component_digest() -> wamn_digest::Digest {
    let base = format!("{}@{}", wamn_fixture_package::PACKAGE_ID, package_version());
    wamn_release::component_declaration::authored_base_digests(&wamn_fixture_package::overlay_root())
        .expect("the overlay manifest authors its base digest")[base.as_str()]
}

fn packages() -> [PackageInput; 2] {
    let base = wamn_fixture_package::package_root();
    let overlay = wamn_fixture_package::overlay_root();
    [
        PackageInput {
            id: wamn_fixture_package::PACKAGE_ID,
            version: package_version(),
            component: "fixture",
            root: base,
            component_bytes: std::env::var_os(BASE_WASM_ENV)
                .map(PathBuf::from)
                .expect("WAMN_EFFECTIVE_RELEASE_BASE_COMPONENT_WASM names the built base component"),
            expected_component_digest: Some(base_component_digest()),
        },
        PackageInput {
            id: wamn_fixture_package::OVERLAY_PACKAGE_ID,
            version: package_version(),
            component: "fixture_overlay",
            root: overlay,
            component_bytes: std::env::var_os(OVERLAY_WASM_ENV)
                .map(PathBuf::from)
                .expect(
                    "WAMN_EFFECTIVE_RELEASE_OVERLAY_COMPONENT_WASM names the built overlay component",
                ),
            expected_component_digest: None,
        },
    ]
}

fn admit_components(
    inputs: &[PackageInput],
) -> (BTreeMap<String, wamn_digest::Digest>, ReleaseComponents) {
    let engine = wamn_engine::build_engine(&[]).expect("build the production admission engine");
    let mut digests = BTreeMap::new();
    let mut release_components = ReleaseComponents::default();
    for input in inputs {
        let base_digests = wamn_release::component_declaration::authored_base_digests(&input.root)
            .unwrap_or_else(|error| {
                panic!("read {}@{} base pins: {error}", input.id, input.version)
            });
        let declaration = wamn_release::component_declaration::render_package_declaration(
            &input.root,
            &input.component.parse().unwrap(),
            &PROJECT_ENV_KEY.parse().unwrap(),
        )
        .unwrap_or_else(|error| panic!("render {}: {error}", input.component));
        let declaration: ComponentDeclaration<ComponentPackageCoordinate> =
            serde_json::from_value(declaration)
                .expect("the package-owned component declaration is strict");
        let (declaration, target) = wamn_component_policy::split_target(declaration);
        let bytes = std::fs::read(&input.component_bytes)
            .unwrap_or_else(|error| panic!("read {}: {error}", input.component_bytes.display()));
        if let Some(expected) = &input.expected_component_digest {
            assert_eq!(
                bytes_sha256(&bytes),
                *expected,
                "{}@{} must use the exact digest pinned by the overlay dependency",
                input.id,
                input.version
            );
        }
        let facts = validate_component_admission(
            &engine,
            &bytes,
            ComponentAdmissionRequest {
                declaration,
                admitted_platform_packages: BTreeSet::from([
                    "wamn:node".to_owned(),
                    "wamn:postgres".to_owned(),
                ]),
                base_digests: base_digests.values().copied().collect(),
            },
        )
        .unwrap_or_else(|error| panic!("admit {}@{} component: {error}", input.id, input.version));
        assert!(
            facts.connections.is_empty(),
            "the POC application components declare no portable store aliases"
        );
        digests.insert(input.id.to_owned(), facts.component_digest);
        let facts = facts.into_admitted(target);
        release_components
            .insert(facts.component, facts.connections)
            .expect("each package names its component once");
    }
    (digests, release_components)
}

#[test]
fn base_and_overlay_project_admitted_routes() {
    let inputs = packages();
    let (admitted_digests, components) = admit_components(&inputs);

    let roots = inputs
        .iter()
        .map(|input| input.root.clone())
        .collect::<Vec<_>>();
    let (manifests, types) = read_package_manifests(&roots)
        .expect("consume exact package manifests and package contracts");
    let packages = inputs
        .iter()
        .map(|input| {
            PackageCoordinate::new(input.id.parse().unwrap(), input.version.clone()).unwrap()
        })
        .collect::<BTreeSet<_>>();
    // Both packages name widget-get-http. The overlay's replaces the base's, so
    // the release carries a route to each package's component.
    let read = |input: &PackageInput| {
        read_package_attachments(std::slice::from_ref(&input.root))
            .expect("read the package attachment document")
    };
    let mut authored_attachments = read(&inputs[0]);
    authored_attachments.extend(read(&inputs[1]));
    let attachments =
        resolve_route_methods(&authored_attachments, &types).expect("resolve the route methods");
    let request = PublishReleaseManifest {
        org: &"effective-release-poc".parse().expect("fixed identifier"),
        project: &"fixture".parse().expect("fixed identifier"),
        env: &ENV.parse().unwrap(),
        packages: &packages,
        attachments: &attachments,
        env_is_disposable: false,
    };

    let first =
        publish_release_with_package_manifests(&request, &components, &manifests, &types).unwrap();
    // Every attachment names a route. Released operation contracts can also
    // have route records without attachments, which open no HTTP endpoint.
    assert!(
        serde_json::to_value(&first.manifest)
            .unwrap()
            .get("workflow")
            .is_none()
    );
    let attached_routes = first
        .manifest
        .attachments
        .values()
        .map(|attachment| {
            (
                &attachment.package_id,
                &attachment.component,
                &attachment.operation,
            )
        })
        .collect::<BTreeSet<_>>();
    let routes = &first.manifest.routes;
    for (package, component, operation) in &attached_routes {
        assert!(routes.iter().any(|route| {
            (&route.package_id, &route.component, &route.operation)
                == (*package, *component, *operation)
        }));
    }
    for route in routes {
        if attached_routes.contains(&(&route.package_id, &route.component, &route.operation)) {
            continue;
        }
        assert!(first.manifest.components.iter().any(|component| {
            component.package_id == route.package_id
                && component.descriptor.component == route.component
                && component
                    .descriptor
                    .operations
                    .contains_key(&route.operation)
        }));
        let contract = &types[&(route.package_id.clone(), route.operation.clone())];
        assert_ne!(contract.type_, OperationType::EventHandler);
        assert_eq!(route.type_, contract.type_);
        assert_eq!(&route.reads, contract.reads());
        assert_eq!(route.revision.as_deref(), contract.revision());
        assert_eq!(route.idempotency, contract.idempotency);
    }
    let type_of = |operation: &str| {
        first
            .manifest
            .routes
            .iter()
            .find(|route| route.operation == operation)
            .map(|route| route.type_)
    };
    assert_eq!(
        type_of(&format!(
            "platform-fixture-overlay:widget/get@{}",
            package_version()
        )),
        Some(OperationType::Get)
    );
    assert_eq!(
        type_of(&format!(
            "platform-fixture:widget/query@{}",
            package_version()
        )),
        Some(OperationType::Query)
    );
    assert_eq!(
        type_of(&format!(
            "platform-fixture:widget/record-batch@{}",
            package_version()
        )),
        Some(OperationType::Command)
    );
    for component in &first.manifest.components {
        assert_eq!(
            component.descriptor.component_digest,
            admitted_digests[component.package_id.as_str()]
        );
    }
}
