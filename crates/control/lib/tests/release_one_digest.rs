//! The §3 check A7 of `docs/plan/platform-deploy.md` (`wamn-snz0.5`): one
//! release published for two environments hashes to one digest.
//!
//! The serving manifest names no tenant, environment, route host or integer
//! id (R1), so the same packages give the same bytes in every environment.
//! The first test assembles the release as the edge bundle does; the second
//! pushes the package once to a local `registry:2` with `push_package` and
//! publishes its verified artifact with `publish_release` into two
//! provisioned environments on PostgreSQL. A push to an unreachable registry
//! writes nothing. The kind half of A7, a request to each environment's
//! route host, runs in `environment_apply_kind`.

use std::path::{Path, PathBuf};

use wamn_catalog::edge_bundle::COMPONENTS_FILE_NAME;
use wamn_catalog::{
    AdmittedComponent, PackageCoordinate, RELEASE_MANIFEST_FILE_NAME, ServingManifest,
};
use wamn_control::apply_package::{ApplyPackageRequest, apply_package};
use wamn_control::dev::edge_bundle::{EdgeScope, PackageRelease, write_package};
use wamn_control::package_artifact::{
    PackageRegistry, PushPackageRequest, open_package, push_package,
};
use wamn_control::provision_org::{ProvisionOrgRequest, provision_org};
use wamn_control::publish_release::{PublishReleaseRequest, publish_release};
use wamn_control::verification_policy::project_environment_policy;
use wamn_control_registry::{Template, project_env_tenant};
use wamn_schema_control::BareSchemaName;

#[path = "support/local_registry.rs"]
mod local_registry;
use local_registry::LocalRegistry;

const ORG: &str = "acme";
const PROJECT: &str = "plant";
const PUBLISHER: &str = "0b5c7f3e-9a41-4d2e-8f6a-1c2d3e4f5a6b";

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// The component of `apps/edge_device` that its `wamn build` compiled. Run
/// `wamn build apps/edge_device` first.
fn device_component() -> PathBuf {
    repository().join("apps/target/wamn/edge_device/components/device.wasm")
}

/// Admit the component and assemble the release of `apps/edge_device` for
/// `environment`, as publish projects it, and return its manifest bytes and
/// admitted component facts.
fn publish(
    environment: &str,
    component: &Path,
    ingress: &Path,
    root: &Path,
) -> (Vec<u8>, Vec<AdmittedComponent>) {
    let out = root.join(environment);
    let _ = std::fs::remove_dir_all(&out);
    write_package(
        &repository().join("apps/edge_device"),
        "device",
        component,
        ingress,
        &PackageRelease {
            scope: EdgeScope {
                org: ORG,
                project: PROJECT,
                environment,
            },
            publisher: PUBLISHER,
        },
        &out,
    )
    .expect("publish the release for the environment");
    let manifest = std::fs::read(out.join(RELEASE_MANIFEST_FILE_NAME)).expect("read the manifest");
    let facts = serde_json::from_slice(
        &std::fs::read(out.join(COMPONENTS_FILE_NAME)).expect("read the component facts"),
    )
    .expect("parse the component facts");
    (manifest, facts)
}

#[test]
fn one_release_published_for_two_environments_has_one_digest() {
    let component = device_component();
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("release-one-digest-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create the scratch directory");
    // The bundle copies the ingress guest without reading it, and the manifest
    // does not depend on it.
    let ingress = root.join("ingress.wasm");
    std::fs::write(&ingress, b"ingress").expect("write the ingress stand-in");

    let (dev, dev_facts) = publish("dev", &component, &ingress, &root);
    let (prod, prod_facts) = publish("prod", &component, &ingress, &root);

    // The two publishes ran in two scopes: each admitted the component for
    // its own environment's tenant.
    assert!(
        dev_facts
            .iter()
            .all(|fact| fact.scope.tenant_id == "acme--plant--dev")
    );
    assert!(
        prod_facts
            .iter()
            .all(|fact| fact.scope.tenant_id == "acme--plant--prod")
    );

    let (dev_manifest, dev_digest) =
        ServingManifest::from_canonical_bytes(&dev).expect("the dev release is canonical");
    let (_, prod_digest) =
        ServingManifest::from_canonical_bytes(&prod).expect("the prod release is canonical");
    println!("A7 dev digest {dev_digest}");
    println!("A7 prod digest {prod_digest}");
    assert_eq!(dev_digest, prod_digest, "one release has one digest");
    assert_eq!(dev, prod, "one release has one canonical encoding");
    assert!(
        !dev_manifest.attachments.is_empty(),
        "the release serves a route, so the check covers the route host"
    );

    let text = String::from_utf8(dev).expect("canonical JSON is UTF-8");
    for absent in [
        "acme--plant",
        "\"tenant-id\"",
        "\"environment\"",
        "\"host\"",
    ] {
        assert!(!text.contains(absent), "the release names {absent}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The run schema that holds the projected environment policy.
const RUN_SCHEMA: &str = "wamn_run";
const RUN_STATE_SQL: &str = include_str!("../../../../deploy/sql/run-state.sql");
const APP_SCHEMA_SQL: &str = include_str!("../../../../deploy/sql/app-schema.sql");

/// Provision `environment` of `acme/plant` in `project` as `publish_release`
/// reads it: the applied package and the projected environment policy, then
/// publish the release of the verified artifact in `registry` to `releases`.
async fn publish_through_publish_release(
    environment: &str,
    registry: &PackageRegistry,
    releases: &str,
    control: &str,
    project: &str,
) -> anyhow::Result<wamn_catalog::ManifestDigest> {
    let package = repository().join("apps/edge_device");
    let tenant = project_env_tenant(ORG, PROJECT, environment);
    let manifest = wamn_schema_generator::PackageManifest::from_slice(
        &std::fs::read(wamn_schema_generator::package_manifest_path(&package))
            .expect("read the package manifest"),
    )
    .expect("parse the package manifest");
    // Apply the verified artifact, whose digest the opener holds, as every
    // release verb does (R3).
    let opened = open_package(registry, &manifest.package.id, &manifest.package.version)
        .await
        .expect("open the verified package artifact");
    apply_package(ApplyPackageRequest {
        package: opened.package_root(),
        database_url: project.to_owned(),
        tenant: tenant.clone(),
    })
    .await
    .expect("apply the package to the environment");
    project_environment_policy(
        control,
        project,
        &BareSchemaName::new(RUN_SCHEMA.to_owned()).expect("the run schema name"),
        ORG,
        &tenant,
        environment,
    )
    .await
    .expect("project the environment policy");
    publish_release(
        PublishReleaseRequest {
            database_url: project.to_owned(),
            control_database_url: control.to_owned(),
            org: ORG.to_owned(),
            tenant,
            environment: environment.to_owned(),
            verified_publisher_principal: PUBLISHER.to_owned(),
            run_schema: RUN_SCHEMA.to_owned(),
            packages: vec![
                PackageCoordinate::new(&manifest.package.id, &manifest.package.version)
                    .expect("the package coordinate"),
            ],
            wirings: Vec::new(),
        },
        registry,
        releases,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn one_release_through_publish_release_for_two_environments_has_one_digest() {
    // The build writes the index that push-package reads the component from.
    device_component();
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("release-one-digest-publish-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create the scratch directory");
    let control = wamn_control_provision::test_database::system();
    let project = wamn_catalog::test_database::tenant();
    project
        .execute(&[
            wamn_control_provision::sql::ensure_db_owner_role_sql(),
            &format!(
                "GRANT CREATE ON DATABASE \"{}\" TO wamn_db_owner",
                project.name()
            ),
            APP_SCHEMA_SQL,
            RUN_STATE_SQL,
        ])
        .expect("install the project floor");
    provision_org(ProvisionOrgRequest {
        org: ORG.to_owned(),
        template: Template::trials(),
        pool: "wamn-pg".to_owned(),
        system_database_url: Some(control.url().to_owned()),
        cluster_namespace: "wamn-system".to_owned(),
        owner_email: None,
    })
    .await
    .expect("provision the org and its policies");

    let local = LocalRegistry::start("release-one-digest");
    local.ready().await;
    let auth_file = root.join("config.json");
    std::fs::write(
        &auth_file,
        format!(
            r#"{{"auths":{{"{}":{{"username":"wamn","password":"wamn"}},"127.0.0.1:9":{{"username":"wamn","password":"wamn"}}}}}}"#,
            local.address
        ),
    )
    .expect("write the registry credential");
    let registry = PackageRegistry {
        artifact_base: format!("{}/wamn/packages", local.address),
        registry_auth_file: auth_file,
        insecure_registry: true,
        oci_ca_paths: Vec::new(),
    };
    // One push makes the one verified artifact both environments publish.
    push_package(&PushPackageRequest {
        package: repository().join("apps/edge_device"),
        registry: registry.clone(),
        component_artifact_base: format!("{}/wamn/components", local.address),
    })
    .await
    .expect("push the package");

    let releases = format!("{}/wamn/releases", local.address);
    let publish = async |environment: &str| {
        publish_through_publish_release(
            environment,
            &registry,
            &releases,
            control.url(),
            project.url(),
        )
        .await
        .expect("publish the release")
        .as_str()
        .to_owned()
    };
    let release_rows = async || -> i64 {
        let (client, connection) = tokio_postgres::connect(project.url(), tokio_postgres::NoTls)
            .await
            .expect("connect to the project database");
        let connection = tokio::spawn(connection);
        let rows = client
            .query_one("SELECT count(*) FROM catalog.releases", &[])
            .await
            .expect("count the release rows")
            .get(0);
        drop(client);
        connection.abort();
        rows
    };
    // A push to an unreachable registry refuses, and the release is not
    // recorded.
    let unreachable = publish_through_publish_release(
        "dev",
        &registry,
        "127.0.0.1:9/wamn/releases",
        control.url(),
        project.url(),
    )
    .await
    .expect_err("a push to an unreachable registry refuses");
    assert!(
        format!("{unreachable:#}").contains("release-manifest-publish-refused"),
        "{unreachable:#}"
    );
    assert_eq!(release_rows().await, 0, "a failed push wrote a release row");

    let dev = publish("dev").await;
    let prod = publish("prod").await;
    println!("A7 publish_release dev digest {dev}");
    println!("A7 publish_release prod digest {prod}");
    assert_eq!(
        dev, prod,
        "one release published for two environments has one digest"
    );
    assert_eq!(
        release_rows().await,
        2,
        "each environment recorded the release after its push"
    );

    let pushed = wamn_runtime::release_manifest_source::ReleaseManifestSource::new(
        &releases,
        true,
        &registry.registry_auth_file,
    )
    .expect("open the release repository")
    .pull_verified(&dev)
    .await
    .expect("pull the pushed release");
    let (manifest, _) =
        ServingManifest::from_canonical_bytes(&pushed).expect("the pushed release is canonical");
    assert!(
        !manifest.attachments.is_empty(),
        "the release serves a route, so the check covers the route host"
    );
    let text = String::from_utf8(pushed).expect("canonical JSON is UTF-8");
    for absent in ["acme--plant", "\"environment\"", "\"host\""] {
        assert!(!text.contains(absent), "the release names {absent}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
