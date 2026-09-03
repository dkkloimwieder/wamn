//! Fresh two-package effective-release proof on disposable PostgreSQL.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use tokio_postgres::{Client, NoTls};
use wamn_catalog::{ComponentDeclaration, PackageCoordinate, ServingAttachment};
use wamn_control_provision::CONTROL_BOOTSTRAP_SQL;
use wamn_runtime::component_admission::{ComponentAdmissionRequest, validate_component_admission};

use super::{
    MintManifestErrorKind, MintReleaseManifest, MintedReleaseManifest, ReleaseWiringTarget,
    mint_release_manifest_with_package_manifests, read_package_manifests,
    resolve_route_host_overlay, sha256, validate_package_weld,
};
use crate::apply_package::{self, ApplyPackageArgs};
use crate::author_wiring::{self, AuthorWiringRequest};
use crate::push_component::{admitted_projection_hash, append_or_verify_admitted_component};

const TENANT: &str = "effective-release-poc";
const ENVIRONMENT: &str = "dev";
const PUBLISHER: &str = "effective-release-poc-publisher";
const RELEASE_ID: i32 = 1;
const PROJECT_URL_ENV: &str = "WAMN_EFFECTIVE_RELEASE_PROJECT_PG_URL";
const CONTROL_URL_ENV: &str = "WAMN_EFFECTIVE_RELEASE_CONTROL_PG_URL";
const BASE_WASM_ENV: &str = "WAMN_EFFECTIVE_RELEASE_BASE_COMPONENT_WASM";
const OVERLAY_WASM_ENV: &str = "WAMN_EFFECTIVE_RELEASE_OVERLAY_COMPONENT_WASM";
const BASE_COMPONENT_DIGEST: &str =
    "sha256:8e04f7f2463c08fe7b969309304328731371c1d0c869a450575106680345a5b8";
const CATALOG_SCHEMA: &str = include_str!("../../../../deploy/sql/catalog-schema.sql");
const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");
const BASE_WIRINGS: [&str; 6] = [
    "purchase_order_get",
    "purchase_order_query",
    "purchase_order_update",
    "receipt_get",
    "receipt_query",
    "receiving_record_receipt",
];
const OVERLAY_WIRINGS: [&str; 6] = [
    "purchase_order_get",
    "purchase_order_update",
    "quality_approve_inspection",
    "quality_create_inspection",
    "quality_load_purchase_order_detail",
    "receiving_record_receipt",
];

struct PackageInput {
    id: &'static str,
    version: &'static str,
    root: PathBuf,
    component_declaration: PathBuf,
    component_bytes: PathBuf,
    expected_component_digest: Option<&'static str>,
    wirings: &'static [&'static str],
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn packages() -> [PackageInput; 2] {
    let root = repository_root();
    let base = root.join("packages/receiving");
    let overlay = root.join("packages/client_acme_receiving");
    [
        PackageInput {
            id: "wamn_receiving",
            version: "1.0.0",
            component_declaration: base.join("publication/components/receiving.json.in"),
            root: base,
            component_bytes: std::env::var_os(BASE_WASM_ENV)
                .map(PathBuf::from)
                .expect("WAMN_EFFECTIVE_RELEASE_BASE_COMPONENT_WASM names the built base component"),
            expected_component_digest: Some(BASE_COMPONENT_DIGEST),
            wirings: &BASE_WIRINGS,
        },
        PackageInput {
            id: "client_acme_receiving",
            version: "3.0.0",
            component_declaration: overlay
                .join("publication/components/client_acme_receiving.json.in"),
            root: overlay,
            component_bytes: std::env::var_os(OVERLAY_WASM_ENV)
                .map(PathBuf::from)
                .expect(
                    "WAMN_EFFECTIVE_RELEASE_OVERLAY_COMPONENT_WASM names the built overlay component",
                ),
            expected_component_digest: None,
            wirings: &OVERLAY_WIRINGS,
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
            "DROP SCHEMA IF EXISTS receiving CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $$ DECLARE role_name text; BEGIN \
               PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
               FOREACH role_name IN ARRAY ARRAY['wamn_app', 'wamn_scenario_author'] LOOP \
                 IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = role_name) THEN \
                   EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                                   NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
                 END IF; \
               END LOOP; \
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
        .expect("scope the control proof session");
}

async fn apply_packages(project_url: &str, inputs: &[PackageInput]) {
    for input in inputs {
        apply_package::run(ApplyPackageArgs {
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
    let engine = wamn_runtime::build_engine(&[]).expect("build the production admission engine");
    let mut digests = BTreeMap::new();
    for input in inputs {
        let mut declaration: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&input.component_declaration).unwrap_or_else(|error| {
                panic!("read {}: {error}", input.component_declaration.display())
            }),
        )
        .unwrap_or_else(|error| panic!("parse {}: {error}", input.component_declaration.display()));
        declaration["scope"]["tenant-id"] = serde_json::Value::String(TENANT.to_owned());
        let declaration: ComponentDeclaration = serde_json::from_value(declaration)
            .expect("the package-owned component declaration is strict");
        let bytes = std::fs::read(&input.component_bytes)
            .unwrap_or_else(|error| panic!("read {}: {error}", input.component_bytes.display()));
        if let Some(expected) = input.expected_component_digest {
            assert_eq!(
                sha256(&bytes),
                expected,
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
        digests.insert(input.id.to_owned(), facts.component.component_digest);
    }
    digests
}

async fn author_wirings(
    project: &mut Client,
    control: &Client,
    inputs: &[PackageInput],
) -> BTreeSet<ReleaseWiringTarget> {
    let mut targets = BTreeSet::new();
    for input in inputs {
        for wiring_id in input.wirings {
            let path = input
                .root
                .join("publication/wirings")
                .join(format!("{wiring_id}.json"));
            let document = author_wiring::read_wiring_document(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let wiring_hash = document.wiring_hash();
            // This seeds only the already-proven steady-state verdict under the
            // document's derived identity. The production journey owns the
            // first transition that writes a gate report.
            control
                .execute(
                    "INSERT INTO wamn_run.gate_reports \
                     (tenant_id, wiring_hash, passed, summary) \
                     VALUES ($1, $2, true, '{\"cases\":0}'::jsonb)",
                    &[&TENANT, &wiring_hash.as_str()],
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
                    package_id: input.id,
                    package_version: input.version,
                    document: &document,
                },
            )
            .await
            .unwrap_or_else(|error| panic!("author {}: {error}", path.display()));
            transaction
                .commit()
                .await
                .expect("commit wiring authorship");
            targets.insert(ReleaseWiringTarget {
                package_id: input.id.to_owned(),
                package_version: input.version.to_owned(),
                wiring_id: document.wiring_id,
                wiring_version: document.version,
            });
        }
    }
    targets
}

async fn mint(
    project: &mut Client,
    request: &MintReleaseManifest<'_>,
    manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    hashes: &BTreeMap<String, String>,
) -> MintedReleaseManifest {
    let transaction = project.transaction().await.expect("begin release mint");
    let release =
        mint_release_manifest_with_package_manifests(&transaction, request, manifests, hashes)
            .await
            .expect("mint the exact fresh two-package release");
    transaction.commit().await.expect("commit release mint");
    release
}

fn prove_typed_weld_refusal(input: &PackageInput) {
    let manifest_bytes = std::fs::read(input.root.join("wamn.json")).unwrap();
    let mut manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    manifest["required_platform_policy_contract"]["state"] = serde_json::json!("unsatisfied");
    let manifest =
        wamn_schema_generator::PackageManifest::from_slice(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
    let mut weld: serde_json::Value = serde_json::from_slice(
        &std::fs::read(input.root.join("generated/package-weld.json")).unwrap(),
    )
    .unwrap();
    weld["required_platform_policy_contract"]["state"] = serde_json::json!("unsatisfied");
    weld["promotion_state"] = serde_json::json!("blocked_unsatisfied_policy_contract");
    let weld = wamn_schema_generator::PackageWeld::from_slice(
        &wamn_execution_contract::canonical_json_bytes(&weld),
    )
    .unwrap();
    let refusal = validate_package_weld(&manifest, &weld)
        .expect_err("an unsatisfied package weld must refuse release mint");
    assert_eq!(
        refusal.kind(),
        MintManifestErrorKind::PolicyContractUnsatisfied
    );
}

#[tokio::test]
#[ignore = "requires two disposable PostgreSQL 18 databases and exact built base/overlay component paths"]
async fn fresh_base_and_overlay_mint_byte_identically_and_refuse_drift() {
    let project_url = std::env::var(PROJECT_URL_ENV)
        .expect("WAMN_EFFECTIVE_RELEASE_PROJECT_PG_URL names disposable PostgreSQL 18");
    let control_url = std::env::var(CONTROL_URL_ENV)
        .expect("WAMN_EFFECTIVE_RELEASE_CONTROL_PG_URL names disposable PostgreSQL 18");
    let inputs = packages();
    prove_typed_weld_refusal(&inputs[0]);

    let (mut project, project_task) = connect(&project_url).await;
    let (control, control_task) = connect(&control_url).await;
    provision_project(&project).await;
    provision_control(&control).await;
    apply_packages(&project_url, &inputs).await;
    let admitted_digests = admit_components(&mut project, &inputs).await;
    let wirings = author_wirings(&mut project, &control, &inputs).await;

    let manifest_paths = inputs
        .iter()
        .map(|input| input.root.join("wamn.json"))
        .collect::<Vec<_>>();
    let (manifests, manifest_hashes) =
        read_package_manifests(&manifest_paths).expect("consume exact package manifests and welds");
    let packages = inputs
        .iter()
        .map(|input| PackageCoordinate::new(input.id, input.version).unwrap())
        .collect::<BTreeSet<_>>();
    let authored_attachments: BTreeMap<String, ServingAttachment> = serde_json::from_slice(
        &std::fs::read(inputs[0].root.join("publication/attachments.json")).unwrap(),
    )
    .unwrap();
    let attachments =
        resolve_route_host_overlay(&authored_attachments, Some("receiving.localhost"))
            .expect("bind the deployment-owned route hostname");
    let request = MintReleaseManifest {
        tenant_id: TENANT,
        effective_release_id: RELEASE_ID,
        environment: ENVIRONMENT,
        verified_publisher_principal: PUBLISHER,
        packages: &packages,
        wirings: &wirings,
        attachments: &attachments,
    };

    let first = mint(&mut project, &request, &manifests, &manifest_hashes).await;
    let second = mint(&mut project, &request, &manifests, &manifest_hashes).await;
    assert_eq!(first.canonical_bytes, second.canonical_bytes);
    assert_eq!(first.digest, second.digest);
    assert_eq!(first.manifest, second.manifest);
    assert_eq!(first.manifest.release.packages, packages);
    assert_eq!(first.manifest.components.len(), 2);
    for component in &first.manifest.components {
        assert_eq!(
            component.digest.as_str(),
            admitted_digests[component.package_id.as_str()]
        );
    }
    let registration =
        &first.manifest.registrations["client_acme_receiving::quality.create_inspection"];
    assert_eq!(registration.package_id, "client_acme_receiving");
    assert_eq!(registration.source_package_id, "wamn_receiving");
    assert_eq!(registration.entity, "receipt");

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
        "client_acme_receiving".to_owned(),
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
    )
    .await
    .expect_err("manifest bytes other than apply-package's exact input must refuse");
    assert_eq!(refusal.kind(), MintManifestErrorKind::PackageManifest);
    assert!(refusal.detail().contains("client_acme_receiving@3.0.0"));
    assert!(refusal.detail().contains("use the exact wamn.json"));
    transaction
        .rollback()
        .await
        .expect("close the refused mint");

    project
        .batch_execute(
            "DROP SCHEMA IF EXISTS receiving CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE;",
        )
        .await
        .expect("clean project proof schemas");
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
        .expect("clean control proof schemas");
    drop(project);
    drop(control);
    project_task.abort();
    control_task.abort();
}
