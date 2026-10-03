//! Live test of `push-package` and `apply-package --package-artifact`
//! (wamn-zua8.3, commit 4) against a local `registry:2` that the test starts
//! and removes, and a disposable control database. It pushes the Receiving
//! package, pushes it again, fetches it, and fetches it once more after the
//! recorded digest changes.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tokio_postgres::NoTls;
use wamn_control::package_artifact::{
    COMPONENT_BUILD_DIRECTORY, COMPONENT_LIST, ListedComponent, PackagePushDisposition,
    PackageRegistry, PackageSource, PushPackageRequest, open_package_source, push_package,
};
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_test_infrastructure::locked_database;

/// A `registry:2` container, removed on drop.
struct LocalRegistry {
    name: String,
    address: String,
}

impl LocalRegistry {
    fn start() -> Self {
        let name = format!("wamn-package-artifact-live-{}", std::process::id());
        let run = Command::new("docker")
            .args([
                "run",
                "-d",
                "--rm",
                "--name",
                &name,
                "-p",
                "127.0.0.1::5000",
                "registry:2",
            ])
            .output()
            .expect("run docker");
        assert!(
            run.status.success(),
            "start registry:2: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        // The guard exists from here, so a failed port read still removes it.
        let mut registry = Self {
            name,
            address: String::new(),
        };
        let port = Command::new("docker")
            .args(["port", &registry.name, "5000/tcp"])
            .output()
            .expect("run docker port");
        String::from_utf8_lossy(&port.stdout)
            .lines()
            .next()
            .expect("registry:2 publishes its port")
            .trim()
            .clone_into(&mut registry.address);
        registry
    }

    async fn ready(&self) {
        let url = format!("http://{}/v2/", self.address);
        for _ in 0..100 {
            if reqwest::get(&url)
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("registry:2 at {} did not answer", self.address);
    }
}

impl Drop for LocalRegistry {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .output();
    }
}

fn receiving_package() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/wamn_receiving")
}

#[tokio::test]
async fn push_repeat_fetch_and_a_refused_fetch() {
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

    let local = LocalRegistry::start();
    local.ready().await;
    let directory =
        std::env::temp_dir().join(format!("wamn-package-artifact-live-{}", std::process::id()));
    std::fs::create_dir(&directory).expect("create the credential directory");
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
    let request = PushPackageRequest {
        package: receiving_package(),
        registry: registry.clone(),
        source_commit: None,
    };

    let first = push_package(&request).await.expect("the first push");
    assert_eq!(first.disposition, PackagePushDisposition::Pushed);
    let repeat = push_package(&request).await.expect("the repeat push");
    assert_eq!(repeat.disposition, PackagePushDisposition::AlreadyPresent);
    assert_eq!(repeat.digest, first.digest);
    let recorded: String = client
        .query_one(
            "SELECT digest FROM catalog.package_artifacts \
              WHERE package_id || '-' || version = $1",
            &[&first.tag],
        )
        .await
        .expect("read the record")
        .get(0);
    assert_eq!(recorded, first.digest);

    let opened = open_package_source(PackageSource::Artifact {
        tag: first.tag.clone(),
        registry: registry.clone(),
    })
    .await
    .expect("fetch with the recorded digest");
    assert_eq!(
        std::fs::read(opened.root().join("generated/wamn.json")).unwrap(),
        std::fs::read(receiving_package().join("generated/wamn.json")).unwrap()
    );
    let listed: Vec<ListedComponent> = serde_json::from_slice(
        &std::fs::read(opened.root().join(COMPONENT_LIST)).expect("read the component list"),
    )
    .expect("decode the component list");
    let built = std::fs::read(
        receiving_package()
            .join(COMPONENT_BUILD_DIRECTORY)
            .join("receiving.wasm"),
    )
    .expect("read the built receiving component");
    assert_eq!(
        listed,
        [ListedComponent {
            name: "receiving".to_owned(),
            sha256: hex::encode(ring::digest::digest(&ring::digest::SHA256, &built)),
        }]
    );
    let unpacked = opened.root().to_path_buf();
    drop(opened);
    assert!(!unpacked.exists(), "the unpacked package is removed");

    let other = format!("sha256:{}", "0".repeat(64));
    client
        .execute("SET session_replication_role = replica", &[])
        .await
        .expect("bypass the immutable trigger as the test superuser");
    client
        .execute(
            "UPDATE catalog.package_artifacts SET digest = $1 \
              WHERE package_id || '-' || version = $2",
            &[&other, &first.tag],
        )
        .await
        .expect("change the recorded digest");
    let refusal = open_package_source(PackageSource::Artifact {
        tag: first.tag.clone(),
        registry,
    })
    .await
    .expect_err("a fetch with another recorded digest refuses");
    let message = format!("{refusal:#}");
    assert!(
        message.contains(&first.digest) && message.contains(&other),
        "the refusal names both digests: {message}"
    );
    std::fs::remove_dir_all(&directory).expect("remove the credential directory");
}
