//! Live test of `push-package` and `apply-package --package-artifact`
//! (wamn-vavs4.1, docs/plan/platform-deploy.md section 7.2) against a local
//! `registry:2` that each test starts and removes, and a disposable control
//! database. The packages are the built packages under `apps/`: run
//! `tools/build-components` first, so `apps/target/components.json` names
//! their components.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tokio_postgres::{Client, NoTls};
use wamn_catalog::{ComponentDescriptor, ComponentPackageScope};
use wamn_control::package_artifact::{
    PackagePushDisposition, PackageRegistry, PackageSource, PushPackageRequest,
    open_package_source, package_artifact_digest, push_package,
};
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_engine::artifact_source::ArtifactSource as _;
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_runtime::registry_credentials::read_registry_credentials;
use wamn_test_infrastructure::locked_database;

#[path = "support/local_registry.rs"]
mod local_registry;
use local_registry::LocalRegistry;

fn apps() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps")
}

/// A provisioned control database, a running registry and the registry inputs
/// for both.
struct Fixture {
    /// Keeps the disposable database until the fixture drops.
    _database: locked_database::LockedDatabase,
    client: Client,
    local: LocalRegistry,
    directory: PathBuf,
    registry: PackageRegistry,
}

impl Fixture {
    async fn start(label: &str) -> Self {
        let url = locked_database::database(wamn_test_postgres::database);
        let (client, connection) = tokio_postgres::connect(&url, NoTls)
            .await
            .expect("connect to the disposable control database");
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
            .batch_execute(
                "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
                   CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                     NOREPLICATION NOBYPASSRLS; \
                 END IF; END $$;",
            )
            .await
            .expect("create the wamn_system role");
        provision_system(&ProvisionSystemRequest {
            system_database_url: url.to_string(),
            platform_domain: "wamn.example.test".to_owned(),
        })
        .await
        .expect("install the control store");

        let local = LocalRegistry::start(label);
        local.ready().await;
        let directory = std::env::temp_dir().join(format!(
            "wamn-package-artifact-live-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory).expect("create the work directory");
        let auth_file = directory.join("config.json");
        std::fs::write(
            &auth_file,
            format!(
                r#"{{"auths":{{"{}":{{"username":"wamn","password":"wamn"}}}}}}"#,
                local.address
            ),
        )
        .expect("write the registry credential");
        let registry = PackageRegistry {
            artifact_base: format!("{}/wamn/packages", local.address),
            registry_auth_file: auth_file,
            insecure_registry: true,
            oci_ca_paths: Vec::new(),
            control_database_url: url.to_string(),
        };
        Self {
            _database: url,
            client,
            local,
            directory,
            registry,
        }
    }

    fn request(&self, package: PathBuf) -> PushPackageRequest {
        PushPackageRequest {
            package,
            registry: self.registry.clone(),
            component_artifact_base: format!("{}/wamn/components", self.local.address),
            source_commit: None,
        }
    }

    /// The descriptor of `component` in the artifact `tag`, pulled again
    /// through the production puller.
    async fn pulled_descriptor(&self, tag: &str, component: &str) -> ComponentDescriptor {
        let opened = open_package_source(PackageSource::Artifact {
            tag: tag.to_owned(),
            registry: self.registry.clone(),
        })
        .await
        .expect("fetch with the recorded digest");
        let descriptor: ComponentDescriptor = serde_json::from_slice(
            &std::fs::read(opened.root().join(format!("descriptors/{component}.json")))
                .expect("read the descriptor"),
        )
        .expect("decode the descriptor");
        let (package_id, version) = tag.rsplit_once('-').expect("a tag names a version");
        let facts = descriptor.clone().into_admitted(
            ComponentPackageScope::new("tenant-a", package_id, version).expect("a scope"),
        );
        let credentials =
            read_registry_credentials(&self.registry.registry_auth_file, &self.local.address)
                .expect("read the registry credential");
        let source = ComponentArtifactSource::new(
            ComponentArtifactSourceConfig::new(
                &format!("{}/wamn/components", self.local.address),
                true,
                Duration::from_secs(30),
            )
            .expect("configure the component source")
            .with_credentials(credentials),
        )
        .expect("build the component source");
        source
            .pull_verified(&facts.component)
            .await
            .expect("the component pulls through pull_verified");
        descriptor
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[tokio::test]
async fn push_repeat_fetch_and_refusals() {
    let fixture = Fixture::start("receiving").await;
    let package = apps().join("wamn_receiving");
    let request = fixture.request(package.clone());

    let first = push_package(&request).await.expect("the first push");
    assert_eq!(first.disposition, PackagePushDisposition::Pushed);
    let repeat = push_package(&request).await.expect("the repeat push");
    assert_eq!(repeat.disposition, PackagePushDisposition::AlreadyPresent);
    assert_eq!(repeat.digest, first.digest);
    let row = fixture
        .client
        .query_one(
            "SELECT digest, verified_at IS NOT NULL FROM catalog.package_artifacts \
              WHERE package_id || '-' || version = $1",
            &[&first.tag],
        )
        .await
        .expect("read the record");
    assert_eq!(row.get::<_, String>(0), first.digest);
    assert!(row.get::<_, bool>(1));

    let opened = open_package_source(PackageSource::Artifact {
        tag: first.tag.clone(),
        registry: fixture.registry.clone(),
    })
    .await
    .expect("fetch with the recorded digest");
    assert_eq!(
        std::fs::read(opened.root().join("generated/wamn.json")).unwrap(),
        std::fs::read(wamn_schema_generator::package_manifest_path(&package)).unwrap()
    );
    assert_eq!(
        package_artifact_digest(opened.root()).expect("pack the unpacked tree"),
        first.digest,
        "the unpacked artifact packs offline to the registry digest"
    );
    let unpacked = opened.root().to_path_buf();
    drop(opened);
    assert!(!unpacked.exists(), "the unpacked package is removed");

    let descriptor = fixture.pulled_descriptor(&first.tag, "receiving").await;
    let built =
        std::fs::read(apps().join("target/virtualized/std-empty-environment/receiving.wasm"))
            .expect("read the built receiving component");
    assert_eq!(
        descriptor.component_digest,
        format!(
            "sha256:{}",
            hex::encode(ring::digest::digest(&ring::digest::SHA256, &built))
        )
    );

    let other = format!("sha256:{}", "0".repeat(64));
    fixture
        .client
        .execute("SET session_replication_role = replica", &[])
        .await
        .expect("bypass the immutable trigger as the test superuser");
    fixture
        .client
        .execute(
            "UPDATE catalog.package_artifacts SET digest = $1 \
              WHERE package_id || '-' || version = $2",
            &[&other, &first.tag],
        )
        .await
        .expect("change the recorded digest");
    let refusal = push_package(&request)
        .await
        .expect_err("a push with another recorded digest refuses");
    let message = format!("{refusal:#}");
    assert!(
        message.contains(&first.digest) && message.contains(&other),
        "the push refusal names both digests: {message}"
    );
    let refusal = open_package_source(PackageSource::Artifact {
        tag: first.tag.clone(),
        registry: fixture.registry.clone(),
    })
    .await
    .expect_err("a fetch with another recorded digest refuses");
    let message = format!("{refusal:#}");
    assert!(
        message.contains(&first.digest) && message.contains(&other),
        "the fetch refusal names both digests: {message}"
    );
}

/// A palette component that the wirings of two packages name is pushed by
/// both and recorded in both artifacts, with no owner table (R3(2)).
#[tokio::test]
async fn a_palette_component_named_by_two_packages_is_recorded_in_both() {
    let fixture = Fixture::start("palette").await;

    // A copy of Receiving whose one wiring names jsonata, beside the build
    // index and the platform declarations it reads.
    let copy = fixture.directory.join("apps");
    std::fs::create_dir(&copy).expect("create the copy");
    for shared in ["platform", "target"] {
        std::os::unix::fs::symlink(apps().join(shared), copy.join(shared))
            .expect("link a shared directory");
    }
    let copied = Command::new("cp")
        .arg("-r")
        .arg(apps().join("wamn_receiving"))
        .arg(&copy)
        .status()
        .expect("run cp");
    assert!(copied.success(), "copy the Receiving package");
    let receiving = copy.join("wamn_receiving");
    std::fs::create_dir_all(receiving.join("publication/wirings")).unwrap();
    std::fs::write(
        receiving.join("publication/wirings/palette.json"),
        br#"{"nodes": {"a": {"component": "jsonata"}}}"#,
    )
    .unwrap();

    let wms = push_package(&fixture.request(apps().join("wamn_wms")))
        .await
        .expect("push WMS");
    let receiving = push_package(&fixture.request(receiving))
        .await
        .expect("push the Receiving copy");
    let recorded: i64 = fixture
        .client
        .query_one("SELECT count(*) FROM catalog.package_artifacts", &[])
        .await
        .expect("count the records")
        .get(0);
    assert_eq!(recorded, 2);
    let in_wms = fixture.pulled_descriptor(&wms.tag, "jsonata").await;
    let in_receiving = fixture.pulled_descriptor(&receiving.tag, "jsonata").await;
    assert_eq!(in_wms.component_digest, in_receiving.component_digest);
    let blob_put = fixture.pulled_descriptor(&wms.tag, "blob-put").await;
    assert_eq!(
        blob_put
            .connections
            .iter()
            .map(|connection| connection.store_alias.as_str())
            .collect::<Vec<_>>(),
        ["labels"],
        "the store alias comes from the wiring"
    );
    let owners: i64 = fixture
        .client
        .query_one("SELECT count(*) FROM catalog.component_digest_owners", &[])
        .await
        .expect("count the owner rows")
        .get(0);
    assert_eq!(owners, 0, "push-package writes no owner row");
}
