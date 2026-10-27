//! Live test of `push-package` and `apply-package --package-artifact`
//! (wamn-vavs4.1, docs/plan/platform-deploy.md section 7.2) against a local
//! `registry:2` that each test starts and removes. No database is read or
//! written, and no `DATABASE_URL` is needed: the registry is the one owner of
//! package facts (R2, R11). The packages are the built packages under
//! `apps/`: run `wamn build` first, so the `build.json` of each package names
//! its components.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use wamn_catalog::{ComponentDescriptor, ComponentPackageScope};
use wamn_control::package_artifact::{
    PackagePushDisposition, PackageRegistry, PackageSource, PushPackageRequest,
    open_package_source, package_artifact_digest, push_package,
};
use wamn_engine::artifact_source::ArtifactSource as _;
use wamn_package_artifact::registry_credentials::read_registry_credentials;
use wamn_package_artifact::{NoPackageArtifact, Repository, pull_package};
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};

#[path = "support/local_registry.rs"]
mod local_registry;
use local_registry::LocalRegistry;

fn apps() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps")
}

#[tokio::test]
async fn push_package_refuses_stale_output_before_reading_its_manifest() -> anyhow::Result<()> {
    use wamn_schema_generator::receipt;
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let package = scratch.path().join("apps/package");
    let output = wamn_schema_generator::output_root(&package);
    std::fs::create_dir_all(&package)?;
    std::fs::create_dir_all(&output)?;
    std::fs::write(package.join("wamn.k"), "# original package\n")?;
    std::fs::write(output.join("wamn.json"), "invalid manifest")?;
    let built = receipt::BuildReceipt {
        schema: receipt::RECEIPT_SCHEMA.to_owned(),
        package: receipt::ReceiptPackage {
            id: "package".into(),
            version: "1.0.0".into(),
        },
        inputs: receipt::ReceiptInputs {
            digest: receipt::input_digest(&package)?,
            files: std::collections::BTreeMap::new(),
        },
        dependencies: Vec::new(),
        generator: receipt::ReceiptGenerator {
            id: "generator".into(),
            toolchain: "toolchain".into(),
            sqlx: "0.9.0".into(),
        },
        outputs: receipt::ReceiptOutputs {
            manifest: "sha256:fixture".into(),
            files: std::collections::BTreeMap::new(),
            components: Vec::new(),
            web: None,
        },
    };
    std::fs::write(output.join(receipt::RECEIPT_FILE), built.to_bytes())?;
    std::fs::write(package.join("wamn.k"), "# edited package\n")?;
    let inputs = receipt::input_digest(&package)?;
    let error = push_package(&PushPackageRequest {
        package: package.clone(),
        registry: PackageRegistry {
            artifact_base: "127.0.0.1:1/packages".into(),
            registry_auth_file: scratch.path().join("absent-auth.json"),
            insecure_registry: true,
            oci_ca_paths: Vec::new(),
        },
        component_artifact_base: "127.0.0.1:1/components".into(),
    })
    .await
    .expect_err("a stale package was accepted");
    let error = format!("{error:#}");
    for detail in [
        package.to_str().unwrap(),
        &built.inputs.digest,
        &inputs,
        "is stale",
        "wamn build",
    ] {
        assert!(error.contains(detail), "{error}");
    }
    Ok(())
}

/// A running registry and its inputs.
struct Fixture {
    local: LocalRegistry,
    directory: PathBuf,
    registry: PackageRegistry,
}

impl Fixture {
    async fn start(label: &str) -> Self {
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
        };
        Self {
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
        .expect("fetch by the tag");
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
    let opened = open_package_source(PackageSource::Artifact {
        tag: first.tag.clone(),
        registry: fixture.registry.clone(),
    })
    .await
    .expect("fetch by the tag");
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
    wamn_schema_generator::receipt::check_current(&package).unwrap();
    let built = std::fs::read(apps().join("target/wamn/wamn_receiving/components/receiving.wasm"))
        .expect("read the built receiving component");
    assert_eq!(
        descriptor.component_digest,
        format!(
            "sha256:{}",
            hex::encode(ring::digest::digest(&ring::digest::SHA256, &built))
        )
    );

    // A reader takes the full digest or the tag, and no prefix.
    let by_digest = Repository::open(&fixture.registry, &first.digest)
        .expect("open by the digest")
        .pull()
        .await
        .expect("pull by the digest")
        .expect("the digest exists");
    let by_tag = pull_package(&fixture.registry, "wamn_receiving", tag_version(&first.tag))
        .await
        .expect("pull by the tag");
    assert_eq!(by_digest.0, first.digest);
    assert_eq!(by_tag.0, first.digest);
    assert_eq!(by_digest.1.len(), by_tag.1.len());
    let prefix = &first.digest[..first.digest.len() - 1];
    assert!(
        Repository::open(&fixture.registry, prefix).is_err(),
        "a digest prefix is refused"
    );
    let absent = pull_package(&fixture.registry, "wamn_receiving", "0.0.0-absent")
        .await
        .expect_err("an absent tag refuses");
    assert_eq!(
        NoPackageArtifact::find(&absent).map(|refusal| refusal.version.as_str()),
        Some("0.0.0-absent"),
        "{absent:#}"
    );
}

/// Tags are immutable. push-package reads the tag `<id>-<version>` before it
/// pushes anything: a tag that holds another digest refuses, names both
/// digests and leaves no new component in the registry (R11).
#[tokio::test]
async fn a_tag_that_holds_another_digest_refuses_before_any_push() {
    use wamn_schema_generator::receipt;

    let fixture = Fixture::start("immutable").await;
    let wms = push_package(&fixture.request(apps().join("wamn_wms")))
        .await
        .expect("push WMS");
    let built = receipt::read_current(
        &apps().join("wamn_receiving"),
        &apps().join("target/wamn/wamn_receiving"),
    )
    .expect("read the Receiving build");
    let tag = format!("{}-{}", built.package.id, built.package.version);
    let receiving_tag = Repository::open(&fixture.registry, &tag).expect("open the tag");
    let (wms_manifest, _) = Repository::open(&fixture.registry, &wms.tag)
        .expect("open the WMS tag")
        .manifest()
        .await
        .expect("read the WMS manifest")
        .expect("WMS exists");
    receiving_tag
        .client
        .push_manifest_raw(
            &receiving_tag.reference,
            wms_manifest,
            reqwest::header::HeaderValue::from_static(oci_client::manifest::OCI_IMAGE_MEDIA_TYPE),
        )
        .await
        .expect("put the WMS manifest under the Receiving tag");
    let components_before = component_tags(&fixture).await;

    let refusal = push_package(&fixture.request(apps().join("wamn_receiving")))
        .await
        .expect_err("a tag that holds another digest refuses");
    let message = format!("{refusal:#}");
    assert!(
        message.contains(&wms.digest)
            && message.contains("but the package packs to sha256:")
            && message.contains("nothing was pushed"),
        "the push refusal names both digests: {message}"
    );
    assert_eq!(
        component_tags(&fixture).await,
        components_before,
        "the refused push left no new component in the registry"
    );
    for component in &built.outputs.components {
        let hex = component.sha256.trim_start_matches("sha256:");
        assert!(
            !components_before.iter().any(|tag| tag == hex),
            "component {} of Receiving is not in the registry",
            component.name
        );
    }
    let held = receiving_tag
        .manifest()
        .await
        .expect("read the tag")
        .expect("the tag exists");
    assert_eq!(held.1, wms.digest, "the refused push wrote nothing");
    let refusal = open_package_source(PackageSource::Artifact {
        tag: tag.clone(),
        registry: fixture.registry.clone(),
    })
    .await
    .expect_err("a tag that holds another package refuses");
    let message = format!("{refusal:#}");
    assert!(
        message.contains(&wms.tag),
        "the fetch refusal names the package the tag holds: {message}"
    );
}

/// The tags of the component repository, sorted.
async fn component_tags(fixture: &Fixture) -> Vec<String> {
    let listed: serde_json::Value = reqwest::get(format!(
        "http://{}/v2/wamn/components/tags/list",
        fixture.local.address
    ))
    .await
    .expect("list the component tags")
    .json()
    .await
    .expect("decode the component tags");
    let mut tags: Vec<String> = listed["tags"]
        .as_array()
        .expect("the registry lists tags")
        .iter()
        .map(|tag| tag.as_str().expect("a tag is a string").to_owned())
        .collect();
    tags.sort();
    tags
}

/// The version of a tag `<package_id>-<version>` whose package id has no `-`.
fn tag_version(tag: &str) -> &str {
    tag.split_once('-').expect("a tag names a version").1
}

/// A palette component that the wirings of two packages name is pushed by
/// both and recorded in both artifacts, with no owner table (R3(2)). No
/// database records either package.
#[tokio::test]
async fn a_palette_component_named_by_two_packages_is_recorded_in_both() {
    let fixture = Fixture::start("palette").await;

    // A copy of Receiving whose one wiring names jsonata, beside the platform
    // declarations it reads.
    let copy = fixture.directory.join("apps");
    std::fs::create_dir(&copy).expect("create the copy");
    std::os::unix::fs::symlink(apps().join("platform"), copy.join("platform"))
        .expect("link the platform declarations");
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
    // The build of the copy: Receiving's, with the inputs of the copy and the
    // jsonata component that WMS's build recorded.
    {
        use wamn_schema_generator::receipt;
        let output = copy.join("target/wamn/wamn_receiving");
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        receipt::check_current(&apps().join("wamn_receiving")).unwrap();
        let copied = Command::new("cp")
            .arg("-r")
            .arg(apps().join("target/wamn/wamn_receiving"))
            .arg(output.parent().unwrap())
            .status()
            .expect("run cp");
        assert!(copied.success(), "copy the Receiving build");
        let wms_output = apps().join("target/wamn/wamn_wms");
        let jsonata = receipt::read_current(&apps().join("wamn_wms"), &wms_output)
            .expect("read the WMS build")
            .outputs
            .components
            .into_iter()
            .find(|component| component.name == "jsonata")
            .expect("the WMS build records jsonata");
        std::fs::copy(wms_output.join(&jsonata.path), output.join(&jsonata.path))
            .expect("copy jsonata");
        let mut built = receipt::read(&output).expect("read the Receiving build");
        built.inputs.digest = receipt::input_digest(&receiving).unwrap();
        built.outputs.components.push(jsonata);
        built
            .outputs
            .components
            .sort_by(|left, right| left.name.cmp(&right.name));
        std::fs::write(output.join(receipt::RECEIPT_FILE), built.to_bytes()).unwrap();
    }

    let wms = push_package(&fixture.request(apps().join("wamn_wms")))
        .await
        .expect("push WMS");
    let receiving = push_package(&fixture.request(receiving))
        .await
        .expect("push the Receiving copy");
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
}
