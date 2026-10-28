//! Connector source identity and registry publication use real artifacts.

use std::path::{Path, PathBuf};

use wamn_control::integration;
use wamn_package_artifact::{PackageRegistry, Repository};

#[path = "support/local_registry.rs"]
mod local_registry;

fn fixture() -> wamn_test_infrastructure::scratch::ScratchRoot {
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let root = scratch.path();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/connector");
    std::fs::create_dir_all(root.join("integrations/fixture/src")).unwrap();
    std::fs::create_dir_all(root.join("tools")).unwrap();
    for file in ["Cargo.toml", "Cargo.lock", "src/lib.rs"] {
        std::fs::copy(
            source.join(file),
            root.join("integrations/fixture").join(file),
        )
        .unwrap();
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tools/guest-rustflags");
    std::fs::copy(script, root.join("tools/guest-rustflags")).unwrap();
    // The scratch repository keeps the same pinned Rust toolchain.
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../rust-toolchain.toml"),
        root.join("rust-toolchain.toml"),
    )
    .unwrap();
    scratch
}

async fn registry(root: &Path, label: &str) -> (local_registry::LocalRegistry, PackageRegistry) {
    let registry = local_registry::LocalRegistry::start(label);
    registry.ready().await;
    let auth = root.join("auth.json");
    std::fs::write(
        &auth,
        format!(
            r#"{{"auths":{{"{}":{{"username":"wamn","password":"wamn"}}}}}}"#,
            registry.address
        ),
    )
    .unwrap();
    let configuration = PackageRegistry {
        artifact_base: format!("{}/wamn/connectors", registry.address),
        registry_auth_file: auth,
        insecure_registry: true,
        oci_ca_paths: Vec::<PathBuf>::new(),
    };
    (registry, configuration)
}

#[tokio::test]
async fn build_digest_tracks_source_bytes_and_survives_another_checkout() {
    let first = fixture();
    let a = integration::build(first.path(), "fixture").await.unwrap();
    let b = integration::build(first.path(), "fixture").await.unwrap();
    assert_eq!(a.digest, b.digest);
    let other = fixture();
    let c = integration::build(other.path(), "fixture").await.unwrap();
    assert_eq!(a.digest, c.digest);
    let source = first.path().join("integrations/fixture/src/lib.rs");
    let bytes = std::fs::read_to_string(&source).unwrap();
    std::fs::write(
        &source,
        format!("{bytes}\n// A source-only edit changes artifact identity.\n"),
    )
    .unwrap();
    let edited = integration::build(first.path(), "fixture").await.unwrap();
    assert_ne!(a.digest, edited.digest);
    assert!(a.directory.join("connector.wasm").is_file());
    assert!(a.directory.join("manifest.json").is_file());
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn publish_pushes_by_digest_and_records_the_catalog_as_control() {
    wamn_test_postgres::require_prerequisites(&["docker"]);
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    database.execute(&["SET ROLE wamn_system; INSERT INTO registry.integrations(code,form,exclusive) VALUES ('fixture','wasm',false)"]).unwrap();
    let fixture = fixture();
    let (_registry, configuration) = registry(fixture.path(), "connector").await;
    let built = integration::build(fixture.path(), "fixture").await.unwrap();
    integration::publish(&built, &configuration, database.url())
        .await
        .unwrap();
    integration::publish(&built, &configuration, database.url())
        .await
        .unwrap();
    let (client, driver) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(driver);
    let row = client.query_one("SELECT digest, count(*) OVER () FROM registry.integration_digest WHERE integration='fixture'", &[]).await.unwrap();
    assert_eq!(row.get::<_, String>(0), built.digest);
    assert_eq!(row.get::<_, i64>(1), 1);
    let repository = Repository::open(&configuration, &built.digest).unwrap();
    let (manifest, digest) = repository.manifest().await.unwrap().unwrap();
    assert_eq!(digest, built.digest);
    let manifest: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
    assert_eq!(manifest["artifactType"], integration::ARTIFACT_TYPE);
    let wasm = wamn_package_artifact::connector::pull(&configuration, "fixture", &built.digest)
        .await
        .unwrap();
    assert_eq!(
        wasm,
        std::fs::read(built.directory.join("connector.wasm")).unwrap()
    );
    assert!(
        wamn_package_artifact::connector::pull(&configuration, "other", &built.digest)
            .await
            .is_err()
    );
    client.batch_execute("SET ROLE wamn_control").await.unwrap();
    assert!(
        client
            .query("SELECT FROM registry.integration_digest", &[])
            .await
            .is_err()
    );
    assert_eq!(
        client
            .query_one(
                "SELECT registry.connector_pin_error($1,$2)",
                &[&built.code, &built.digest]
            )
            .await
            .unwrap()
            .get::<_, Option<String>>(0),
        None
    );
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn publish_refuses_an_unknown_integration_without_creating_catalog_rows() {
    wamn_test_postgres::require_prerequisites(&["docker"]);
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let fixture = fixture();
    let (_registry, configuration) = registry(fixture.path(), "unknown-connector").await;
    let built = integration::build(fixture.path(), "fixture").await.unwrap();
    let error = integration::publish(&built, &configuration, database.url())
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("integration_not_found"),
        "{error:#}"
    );
    assert_eq!(database.execute(&["SELECT (SELECT count(*) FROM registry.integrations WHERE code='fixture'), (SELECT count(*) FROM registry.integration_digest)"]).unwrap().trim(), "0|0");
}

#[tokio::test]
async fn publish_catalog_refuses_host_and_native_integrations() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    database.execute(&["SET ROLE wamn_system; INSERT INTO registry.integrations(code,form,exclusive) VALUES ('native_fixture','native',false)"]).unwrap();
    let (client, driver) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(driver);
    client.batch_execute("SET ROLE wamn_control").await.unwrap();
    let digest = format!("sha256:{}", "a".repeat(64));
    for code in ["http", "native_fixture"] {
        let error = client
            .query_one(
                "SELECT registry.publish_integration($1,$2)",
                &[&code, &digest],
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.as_db_error().unwrap().message(),
            "integration_form_mismatch"
        );
    }
    assert_eq!(
        database
            .execute(&["SELECT count(*) FROM registry.integration_digest"])
            .unwrap()
            .trim(),
        "0"
    );
}
