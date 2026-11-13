//! Integration source identity and registry publication use real artifacts.

use sqlx::{Connection as _, Row as _, Statement as _};
use std::path::{Path, PathBuf};

use wamn_package_artifact::{PackageRegistry, Repository};
use wamn_package_control::integration;

#[path = "../../environment/tests/support/local_registry.rs"]
mod local_registry;

fn fixture() -> wamn_test_infrastructure::scratch::ScratchRoot {
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let root = scratch.path();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integration");
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
        artifact_base: format!("{}/wamn/integrations", registry.address),
        credentials: None,
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
    assert!(a.directory.join("integration.wasm").is_file());
    assert!(a.directory.join("manifest.json").is_file());
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(a.directory.join("config.json")).unwrap()).unwrap();
    assert_eq!(config["runtime_type"], "wasm");
    assert!(config.get("form").is_none());
}

#[tokio::test]
#[ignore = "requires: bin:docker"]
async fn publish_pushes_by_digest_and_records_the_catalog_as_control() {
    wamn_test_postgres::require_ignored_prerequisites("requires: bin:docker");
    let _lock = wamn_test_postgres::lock();
    let mut database = wamn_control_provision::test_database::system();
    database.execute(&["SET ROLE wamn_system; INSERT INTO registry.integration_type(code,runtime_type,exclusive) VALUES ('fixture','wasm',false)"]).unwrap();
    let fixture = fixture();
    let (_registry, configuration) = registry(fixture.path(), "integration").await;
    let built = integration::build(fixture.path(), "fixture").await.unwrap();
    integration::publish(
        &built,
        &configuration,
        database.url(),
        &integration::Publication {
            version: 1,
            parameter_schema: serde_json::json!({"type":"object","properties":{}}),
            write_interfaces: Vec::new(),
            operations: "test:fixture/client".into(),
        },
    )
    .await
    .unwrap();
    integration::publish(
        &built,
        &configuration,
        database.url(),
        &integration::Publication {
            version: 1,
            parameter_schema: serde_json::json!({"type":"object","properties":{}}),
            write_interfaces: Vec::new(),
            operations: "test:fixture/client".into(),
        },
    )
    .await
    .unwrap_err();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();

    let row = sqlx::query("SELECT digest, count(*) OVER (), exports FROM registry.integration_type_version WHERE integration_type='fixture'").fetch_one(&mut client).await.unwrap();
    assert_eq!(row.get::<String, _>(0), built.digest);
    assert_eq!(row.get::<i64, _>(1), 1);
    assert_eq!(
        row.get::<Vec<String>, _>(2),
        ["wamn:integration/health@0.1.0"]
    );
    let repository = Repository::open(&configuration, &built.digest).unwrap();
    let (manifest, digest) = repository.manifest().await.unwrap().unwrap();
    assert_eq!(digest, built.digest);
    let manifest: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
    assert_eq!(manifest["artifactType"], integration::ARTIFACT_TYPE);
    let wasm = wamn_package_artifact::integration::pull(&configuration, "fixture", &built.digest)
        .await
        .unwrap();
    assert_eq!(
        wasm,
        std::fs::read(built.directory.join("integration.wasm")).unwrap()
    );
    assert!(
        wamn_package_artifact::integration::pull(&configuration, "other", &built.digest)
            .await
            .is_err()
    );
    sqlx::raw_sql("SET ROLE wamn_control")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    assert!(
        sqlx::query("SELECT FROM registry.integration_type_version")
            .fetch_all(&mut client)
            .await
            .is_err()
    );
    let catalog: serde_json::Value = sqlx::query(
        "SELECT registry.configure_integration_type('fixture','integration_type.list','{}'::jsonb)",
    )
    .fetch_one(&mut client)
    .await
    .unwrap()
    .get(0);
    let published = catalog["integration_types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["code"] == built.code)
        .unwrap();
    assert_eq!(published["versions"][0]["digest"], built.digest);
}

#[tokio::test]
#[ignore = "requires: bin:docker"]
async fn publish_refuses_an_unknown_integration_without_creating_catalog_rows() {
    wamn_test_postgres::require_ignored_prerequisites("requires: bin:docker");
    let _lock = wamn_test_postgres::lock();
    let mut database = wamn_control_provision::test_database::system();
    let fixture = fixture();
    let (_registry, configuration) = registry(fixture.path(), "unknown-integration").await;
    let built = integration::build(fixture.path(), "fixture").await.unwrap();
    let error = integration::publish(
        &built,
        &configuration,
        database.url(),
        &integration::Publication {
            version: 1,
            parameter_schema: serde_json::json!({"type":"object","properties":{}}),
            write_interfaces: Vec::new(),
            operations: "test:fixture/client".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("integration_not_found"),
        "{error:#}"
    );
    assert_eq!(database.execute(&["SELECT (SELECT count(*) FROM registry.integration_type WHERE code='fixture'), (SELECT count(*) FROM registry.integration_type_version WHERE integration_type='fixture')"]).unwrap().trim(), "0|0");
}
