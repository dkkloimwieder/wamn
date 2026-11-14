//! Live test of `push-package` and `apply-package --package-artifact`
//! (wamn-vavs4.1, docs/operations/building.md#reading-a-pushed-package) against a local
//! `registry:2` that each test starts and removes. No database is read or
//! written, and no `DATABASE_URL` is needed: the registry is the one owner of
//! package facts (R2, R11). The packages are the built packages under
//! `apps/`: run `wamn build` first, so the `build.json` of each package names
//! its components.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use wamn_catalog::{ComponentDescriptor, ComponentPackageCoordinate};
use wamn_digest::bytes_sha256;
use wamn_engine::artifact_source::ArtifactSource as _;
use wamn_package_artifact::registry_credentials::read_registry_credentials;
use wamn_package_artifact::{NoPackageArtifact, Repository, pull_package};
use wamn_package_control::package_artifact::{
    PackagePushDisposition, PackageRegistry, PackageSource, PushPackageRequest,
    open_package_source, package_artifact_digest, push_package,
};
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};

#[path = "../../environment/tests/support/local_registry.rs"]
mod local_registry;
use local_registry::LocalRegistry;

fn apps() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps")
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
            credentials: None,
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
            ComponentPackageCoordinate::new("org-a", package_id, version).expect("a target"),
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
        std::fs::read(wamn_compiler::package_manifest_path(&package)).unwrap()
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
    assert_eq!(by_digest.digest, first.digest);
    assert_eq!(by_tag.0, first.digest);
    assert!(
        by_digest
            .layers
            .iter()
            .map(|layer| bytes_sha256(&layer.bytes))
            .eq(by_tag.1.iter().map(|layer| bytes_sha256(&layer.bytes)))
    );
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
    use wamn_compiler::build_record;

    let fixture = Fixture::start("immutable").await;
    let wms = push_package(&fixture.request(apps().join("wamn_wms")))
        .await
        .expect("push WMS");
    let built = build_record::read(&apps().join("target/wamn/wamn_receiving"))
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
