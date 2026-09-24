//! Fresh two-package effective-release test on disposable PostgreSQL.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use tokio_postgres::{Client, NoTls};
use wamn_catalog::{AdmittedComponent, ComponentDeclaration, OperationKind, PackageCoordinate};
use wamn_control_provision::CONTROL_BOOTSTRAP_SQL;
use wamn_engine::component_admission::{ComponentAdmissionRequest, validate_component_admission};

use super::{
    DependencyDigestRule, MintManifestErrorKind, MintReleaseManifest, MintedReleaseManifest,
    ReleaseWiringTarget, effect_free_operation_dependencies,
    mint_release_manifest_with_package_manifests, read_package_attachments, read_package_manifests,
    resolve_route_host_overlay, sha256, validate_package_metadata,
};
use crate::apply_package::{self, ApplyPackageRequest};
use crate::author_wiring::{self, AuthorWiringRequest};
use crate::push_component::{admitted_projection_hash, append_or_verify_admitted_component};

const TENANT: &str = "effective-release-poc";
const ENVIRONMENT: &str = "dev";
const PUBLISHER: &str = "effective-release-poc-publisher";
const RELEASE_ID: i32 = 1;
const BASE_WASM_ENV: &str = "WAMN_EFFECTIVE_RELEASE_BASE_COMPONENT_WASM";
const OVERLAY_WASM_ENV: &str = "WAMN_EFFECTIVE_RELEASE_OVERLAY_COMPONENT_WASM";
const CATALOG_SCHEMA: &str = wamn_catalog::CATALOG_SCHEMA_SQL;
const APP_SCHEMA: &str = include_str!("../../../../../deploy/sql/app-schema.sql");
const PACKAGE_VERSION: &str = "1.0.0";

struct PackageInput {
    id: &'static str,
    version: &'static str,
    root: PathBuf,
    component_declaration: PathBuf,
    component_bytes: PathBuf,
    expected_component_digest: Option<String>,
}

/// The base component digest, read from the ONE file that authors it.
///
/// wamn-10yt.50: this test used to restate the same `sha256:` literal the
/// overlay manifest pins, which is a third copy of a value that must be one.
fn base_component_digest() -> String {
    let base = format!("{}@{PACKAGE_VERSION}", wamn_fixture_package::PACKAGE_ID);
    crate::component_declaration::authored_base_digests(&wamn_fixture_package::overlay_root())
        .expect("the overlay manifest authors its base digest")[base.as_str()]
    .to_string()
}

fn packages() -> [PackageInput; 2] {
    let base = wamn_fixture_package::package_root();
    let overlay = wamn_fixture_package::overlay_root();
    [
        PackageInput {
            id: wamn_fixture_package::PACKAGE_ID,
            version: PACKAGE_VERSION,
            component_declaration: base.join("publication/components/fixture.json.in"),
            root: base,
            component_bytes: std::env::var_os(BASE_WASM_ENV)
                .map(PathBuf::from)
                .expect("WAMN_EFFECTIVE_RELEASE_BASE_COMPONENT_WASM names the built base component"),
            expected_component_digest: Some(base_component_digest()),
        },
        PackageInput {
            id: wamn_fixture_package::OVERLAY_PACKAGE_ID,
            version: PACKAGE_VERSION,
            component_declaration: overlay
                .join("publication/components/fixture_overlay.json.in"),
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

async fn connect(url: &str) -> (Client, tokio::task::JoinHandle<()>) {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to disposable PostgreSQL");
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    (client, task)
}

async fn provision_project(project: &Client) {
    project
        .batch_execute(
            // `apply_package` narrows migration authority to `wamn_db_owner`
            // (48367402), so that role must exist and must own this database
            // or the migrations are refused for want of CREATE on it. The test
            // creates every role it needs; it takes no out-of-band setup.
            "DROP SCHEMA IF EXISTS inventory CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $$ DECLARE role_name text; BEGIN \
               PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
               FOREACH role_name IN ARRAY \
                   ARRAY['wamn_app', 'wamn_scenario_author', 'wamn_db_owner'] LOOP \
                 IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = role_name) THEN \
                   EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                                   NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
                 END IF; \
               END LOOP; \
             END $$; \
             DO $$ BEGIN \
               EXECUTE format('ALTER DATABASE %I OWNER TO wamn_db_owner', \
                              pg_catalog.current_database()); \
             END $$;",
        )
        .await
        .expect("reset the project schemas and prerequisite roles");
    project
        .batch_execute(&format!("{CATALOG_SCHEMA}\n{APP_SCHEMA}"))
        .await
        .expect("install the production project schemas");
}

async fn provision_control(control: &Client) {
    control
        .batch_execute(
            "DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS identity CASCADE; \
             DO $$ DECLARE role_name text; BEGIN \
               PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
               FOREACH role_name IN ARRAY ARRAY['wamn_system', 'wamn_control_author', 'wamn_app'] LOOP \
                 IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = role_name) THEN \
                   EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                                   NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
                 END IF; \
               END LOOP; \
             END $$; \
             DO $$ BEGIN \
               EXECUTE format('REVOKE CONNECT ON DATABASE %I FROM PUBLIC', \
                              pg_catalog.current_database()); \
             END $$;",
        )
        .await
        .expect("reset the control schemas and prerequisite roles");
    for stage in CONTROL_BOOTSTRAP_SQL {
        control
            .batch_execute(stage)
            .await
            .expect("install the production control bootstrap");
    }
    control
        .query_one("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
        .await
        .expect("scope the control test session");
}

async fn apply_packages(project_url: &str, inputs: &[PackageInput]) {
    for input in inputs {
        apply_package::apply_package(ApplyPackageRequest {
            package: input.root.clone(),
            database_url: project_url.to_owned(),
            tenant: TENANT.to_owned(),
        })
        .await
        .unwrap_or_else(|error| panic!("apply {}@{}: {error:#}", input.id, input.version));
    }
}

async fn admit_components(
    project: &mut Client,
    inputs: &[PackageInput],
) -> BTreeMap<String, String> {
    let engine = wamn_engine::build_engine(&[]).expect("build the production admission engine");
    let mut digests = BTreeMap::new();
    // The test admits packages in dependency order, base before overlay, so
    // the fact a dependency resolves to is already in hand when the component
    // that declares the dependency reaches admission.
    let mut component_facts: BTreeMap<(String, String), Vec<AdmittedComponent>> = BTreeMap::new();
    for input in inputs {
        // The template leaves its base dependency digest as a placeholder, so
        // the render -- not a tenant substitution -- is what makes it a
        // declaration (wamn-10yt.50).
        let base_digests = crate::component_declaration::authored_base_digests(&input.root)
            .unwrap_or_else(|error| {
                panic!("read {}@{} base pins: {error}", input.id, input.version)
            });
        let declaration = crate::component_declaration::render_declaration_document(
            &input.component_declaration,
            TENANT,
            &base_digests,
        )
        .unwrap_or_else(|error| {
            panic!("render {}: {error}", input.component_declaration.display())
        });
        let declaration: ComponentDeclaration = serde_json::from_value(declaration)
            .expect("the package-owned component declaration is strict");
        let bytes = std::fs::read(&input.component_bytes)
            .unwrap_or_else(|error| panic!("read {}: {error}", input.component_bytes.display()));
        if let Some(expected) = &input.expected_component_digest {
            assert_eq!(
                sha256(&bytes),
                *expected,
                "{}@{} must use the exact digest pinned by the overlay dependency",
                input.id,
                input.version
            );
        }
        let effect_free_operation_dependencies = effect_free_operation_dependencies(
            &declaration,
            &component_facts,
            DependencyDigestRule::Declared,
        );
        let facts = validate_component_admission(
            &engine,
            &bytes,
            ComponentAdmissionRequest {
                declaration,
                admitted_platform_packages: BTreeSet::from([
                    "wamn:node".to_owned(),
                    "wamn:postgres".to_owned(),
                ]),
                effect_free_operation_dependencies,
            },
        )
        .unwrap_or_else(|error| panic!("admit {}@{} component: {error}", input.id, input.version));
        assert!(
            facts.connections.is_empty(),
            "the POC application components declare no portable store aliases"
        );
        let projection_hash = admitted_projection_hash(&facts.component, &[])
            .expect("hash the admitted component projection");
        let transaction = project
            .transaction()
            .await
            .expect("begin component fact persistence");
        transaction
            .query_one(super::CLAIM_TENANT_SQL, &[&TENANT])
            .await
            .expect("claim the component tenant");
        append_or_verify_admitted_component(&transaction, &facts.component, &projection_hash)
            .await
            .expect("persist the byte-admitted component fact");
        transaction
            .commit()
            .await
            .expect("commit the byte-admitted component fact");
        digests.insert(
            input.id.to_owned(),
            facts.component.component_digest.clone(),
        );
        let scope = (
            facts.component.scope.package_id.clone(),
            facts.component.scope.package_version.clone(),
        );
        component_facts
            .entry(scope)
            .or_default()
            .push(facts.component);
    }
    digests
}

/// Author one base-package wiring of the fixture get operation, with no edges.
async fn author_one_node_wiring(project: &mut Client, control: &Client) -> ReleaseWiringTarget {
    let document = wamn_catalog::WiringDocument::parse(&serde_json::json!({
        "format-version": "0.1",
        "wiring-id": "widget_get",
        "version": 1,
        "entry": "operation",
        "nodes": {
            "operation": {
                "component": "fixture",
                "interface-version": "0.1.0",
                "operation": "platform-fixture:widget/get@1.0.0",
                "terminal": "respond"
            }
        }
    }))
    .expect("the one-node wiring parses");
    // This seeds only the already-checked steady-state verdict under the
    // document's derived identity. The production journey owns the first
    // transition that writes a gate report.
    control
        .execute(
            "INSERT INTO wamn_run.gate_reports \
             (tenant_id, wiring_hash, passed, summary) \
             VALUES ($1, $2, true, '{\"cases\":0}'::jsonb)",
            &[&TENANT, &document.wiring_hash().as_str()],
        )
        .await
        .expect("record the exact wiring's already-owned green verdict");
    let transaction = project
        .transaction()
        .await
        .expect("begin wiring authorship");
    author_wiring::author_wiring(
        control,
        &transaction,
        &AuthorWiringRequest {
            tenant_id: TENANT,
            package_id: wamn_fixture_package::PACKAGE_ID,
            package_version: PACKAGE_VERSION,
            document: &document,
        },
    )
    .await
    .expect("author the one-node wiring");
    transaction
        .commit()
        .await
        .expect("commit wiring authorship");
    ReleaseWiringTarget {
        package_id: wamn_fixture_package::PACKAGE_ID.to_owned(),
        package_version: PACKAGE_VERSION.to_owned(),
        wiring_id: document.wiring_id,
        wiring_version: document.version,
    }
}

async fn mint(
    project: &mut Client,
    request: &MintReleaseManifest<'_>,
    manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    hashes: &BTreeMap<String, String>,
    kinds: &super::RouteKinds,
) -> MintedReleaseManifest {
    let transaction = project.transaction().await.expect("begin release mint");
    let release = mint_release_manifest_with_package_manifests(
        &transaction,
        request,
        manifests,
        hashes,
        kinds,
    )
    .await
    .expect("mint the exact fresh two-package release");
    transaction.commit().await.expect("commit release mint");
    release
}

fn assert_typed_metadata_refusal(input: &PackageInput) {
    let manifest_bytes = std::fs::read(input.root.join("wamn.json")).unwrap();
    let mut manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    manifest["required_platform_policy_contract"]["state"] = serde_json::json!("unsatisfied");
    let manifest =
        wamn_schema_generator::PackageManifest::from_slice(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
    let mut metadata: serde_json::Value = serde_json::from_slice(
        &std::fs::read(input.root.join("generated/package-weld.json")).unwrap(),
    )
    .unwrap();
    metadata["required_platform_policy_contract"]["state"] = serde_json::json!("unsatisfied");
    metadata["promotion_state"] = serde_json::json!("blocked_unsatisfied_policy_contract");
    let metadata = wamn_schema_generator::GeneratedPackageMetadata::from_slice(
        &wamn_execution_contract::canonical_json_bytes(&metadata),
    )
    .unwrap();
    let refusal = validate_package_metadata(&manifest, &metadata)
        .expect_err("unsatisfied package metadata must refuse release mint");
    assert_eq!(
        refusal.kind(),
        MintManifestErrorKind::PolicyContractUnsatisfied
    );
}

#[tokio::test]
#[ignore = "requires: WAMN_EFFECTIVE_RELEASE_BASE_COMPONENT_WASM, WAMN_EFFECTIVE_RELEASE_OVERLAY_COMPONENT_WASM"]
async fn fresh_base_and_overlay_mint_byte_identically_and_refuse_drift() {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_EFFECTIVE_RELEASE_BASE_COMPONENT_WASM",
        "WAMN_EFFECTIVE_RELEASE_OVERLAY_COMPONENT_WASM",
    ]);
    let inputs = packages();
    assert_typed_metadata_refusal(&inputs[0]);
    let _lock = wamn_test_postgres::lock();
    let project_database = wamn_test_postgres::database();
    let control_database = wamn_test_postgres::database();
    let project_url = project_database.url().to_owned();
    let control_url = control_database.url().to_owned();

    let (mut project, project_task) = connect(&project_url).await;
    let (control, control_task) = connect(&control_url).await;
    provision_project(&project).await;
    provision_control(&control).await;
    apply_packages(&project_url, &inputs).await;
    let admitted_digests = admit_components(&mut project, &inputs).await;
    let wirings = BTreeSet::new();

    let manifest_paths = inputs
        .iter()
        .map(|input| input.root.join("wamn.json"))
        .collect::<Vec<_>>();
    let (manifests, manifest_hashes, kinds) = read_package_manifests(&manifest_paths)
        .expect("consume exact package manifests and package contracts");
    let packages = inputs
        .iter()
        .map(|input| PackageCoordinate::new(input.id, input.version).unwrap())
        .collect::<BTreeSet<_>>();
    // Both packages name widget-get-http. The overlay's replaces the base's, so
    // the release carries a route to each package's component.
    let read = |input: &PackageInput| {
        read_package_attachments(
            &[input.root.join("publication/attachments.json")],
            &manifest_paths,
        )
        .expect("read the package attachment document")
    };
    let mut authored_attachments = read(&inputs[0]);
    authored_attachments.extend(read(&inputs[1]));
    let attachments = resolve_route_host_overlay(&authored_attachments, Some("fixture.localhost"))
        .expect("bind the deployment-owned route hostname");
    let request = MintReleaseManifest {
        tenant_id: TENANT,
        effective_release_id: RELEASE_ID,
        environment: ENVIRONMENT,
        verified_publisher_principal: PUBLISHER,
        packages: &packages,
        wirings: &wirings,
        attachments: &attachments,
        environment_is_disposable: false,
    };

    let first = mint(&mut project, &request, &manifests, &manifest_hashes, &kinds).await;
    let second = mint(&mut project, &request, &manifests, &manifest_hashes, &kinds).await;
    assert_eq!(first.canonical_bytes, second.canonical_bytes);
    assert_eq!(first.digest, second.digest);
    assert_eq!(first.manifest, second.manifest);
    assert_eq!(first.manifest.release.packages, packages);
    assert_eq!(first.manifest.components.len(), 2);
    // Every attachment is a route: the release has routes and no wiring, and
    // each route carries the kind its generated contract names.
    assert!(first.manifest.wirings.is_empty());
    assert_eq!(first.manifest.routes.len(), attachments.len());
    let kind_of = |operation: &str| {
        first
            .manifest
            .routes
            .iter()
            .find(|route| route.operation == operation)
            .map(|route| route.kind)
    };
    assert_eq!(
        kind_of("platform-fixture-overlay:widget/get@1.0.0"),
        Some(OperationKind::Get)
    );
    assert_eq!(
        kind_of("platform-fixture:widget/query@1.0.0"),
        Some(OperationKind::Query)
    );
    assert_eq!(
        kind_of("platform-fixture:widget/record-batch@1.0.0"),
        Some(OperationKind::Command)
    );
    let route_members: i64 = project
        .query_one(
            "SELECT count(*) FROM catalog.release_components \
             WHERE tenant_id = $1 AND effective_release_id = $2 \
               AND route_operation IS NOT NULL AND wiring_id IS NULL",
            &[&TENANT, &RELEASE_ID],
        )
        .await
        .expect("count the route members")
        .get(0);
    assert_eq!(
        usize::try_from(route_members).unwrap(),
        first.manifest.routes.len()
    );
    for component in &first.manifest.components {
        assert_eq!(
            component.digest.as_str(),
            admitted_digests[component.package_id.as_str()]
        );
    }
    // The fixture declares no event handler. The unit test
    // serving_registration_is_derived_from_the_exact_handler_and_unique_entry_wiring
    // states the registration a handler derives.

    let stored: Vec<u8> = project
        .query_one(
            "SELECT canonical_bytes FROM catalog.release_manifest_v3_snapshots \
             WHERE tenant_id = $1 AND effective_release_id = $2",
            &[&TENANT, &RELEASE_ID],
        )
        .await
        .expect("read the frozen release artifact")
        .get(0);
    assert_eq!(stored, first.canonical_bytes);
    let snapshot_count: i64 = project
        .query_one(
            "SELECT count(*) FROM catalog.release_manifest_v3_snapshots \
             WHERE tenant_id = $1",
            &[&TENANT],
        )
        .await
        .expect("count immutable release artifacts")
        .get(0);
    assert_eq!(snapshot_count, 1);

    let mut drifted_hashes = manifest_hashes.clone();
    drifted_hashes.insert(
        wamn_fixture_package::OVERLAY_PACKAGE_ID.to_owned(),
        format!("sha256:{}", "f".repeat(64)),
    );
    let transaction = project.transaction().await.expect("begin refused mint");
    let refusal = mint_release_manifest_with_package_manifests(
        &transaction,
        &MintReleaseManifest {
            effective_release_id: RELEASE_ID + 1,
            ..request
        },
        &manifests,
        &drifted_hashes,
        &kinds,
    )
    .await
    .expect_err("manifest bytes other than apply-package's exact input must refuse");
    assert_eq!(refusal.kind(), MintManifestErrorKind::PackageManifest);
    assert!(refusal.detail().contains(&format!(
        "{}@{PACKAGE_VERSION}",
        wamn_fixture_package::OVERLAY_PACKAGE_ID
    )));
    assert!(refusal.detail().contains("use the exact wamn.json"));
    transaction
        .rollback()
        .await
        .expect("close the refused mint");

    // A graph with no edges is a route, so publish refuses a one-node wiring
    // that no registration names.
    let one_node = BTreeSet::from([author_one_node_wiring(&mut project, &control).await]);
    let transaction = project.transaction().await.expect("begin refused mint");
    let refusal = mint_release_manifest_with_package_manifests(
        &transaction,
        &MintReleaseManifest {
            effective_release_id: RELEASE_ID + 2,
            wirings: &one_node,
            ..request
        },
        &manifests,
        &manifest_hashes,
        &kinds,
    )
    .await
    .expect_err("a one-node wiring must refuse");
    assert_eq!(refusal.kind(), MintManifestErrorKind::Wiring);
    assert!(refusal.detail().contains("has no edges"), "{refusal}");
    transaction
        .rollback()
        .await
        .expect("close the refused mint");

    project
        .batch_execute(
            "DROP SCHEMA IF EXISTS inventory CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE;",
        )
        .await
        .expect("clean project test schemas");
    control
        .batch_execute(
            "DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS identity CASCADE;",
        )
        .await
        .expect("clean control test schemas");
    drop(project);
    drop(control);
    project_task.abort();
    control_task.abort();
}
