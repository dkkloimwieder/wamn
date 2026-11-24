//! Check each application's Helm release record against the Secret size limit.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result, bail, ensure};
use base64::Engine as _;
use wamn_component_policy::PackageManifest;
use wamn_digest::{Digest, bytes_sha256};
use wamn_environment::environment::document::EnvironmentDocument;
use wamn_package::PackageName;
use wamn_package::manifest::deploy::PublishedComponent;

/// The Kubernetes Secret limit; the Helm Secret driver stores one release per Secret.
const RECORD_LIMIT: usize = 1_048_576;
const ORG: &str = "acme";

/// Check the release record of every package under `apps/`.
///
/// Registry digests cannot be computed offline, so each one is a
/// placeholder of the same length.
pub(super) async fn run(root: &Path) -> Result<()> {
    let mut packages = BTreeMap::new();
    for path in crate::prepare_tests::applications(root)? {
        let source = wamn_build::prepare_source(&path)
            .await
            .with_context(|| format!("read application {}", path.display()))?;
        let id = source.package().id.clone();
        let output =
            wamn_build::package_output(&wamn_build::default_output_root(source.root()), &id);
        ensure!(
            output.is_dir(),
            "{id} has no build output at {}; run `cargo xtask prepare-tests`",
            output.display()
        );
        packages.insert(id, published_manifest(&output));
    }
    let mut failed = Vec::new();
    let mut published = BTreeMap::new();
    for (id, manifest) in packages {
        match manifest {
            Ok(manifest) => {
                published.insert(id, manifest);
            }
            Err(error) => {
                println!("{id}: verify the package: {error:#}");
                failed.push(id);
            }
        }
    }

    let directory = tempfile::tempdir()?;
    let chart = stamp_chart(root, &directory.path().join("chart"))?;
    for id in published.keys() {
        match record_bytes(&published, id, &chart, directory.path()).await {
            Ok(bytes) => {
                println!("{id}: {bytes} bytes of the {RECORD_LIMIT} limit");
                if bytes > RECORD_LIMIT {
                    failed.push(id.clone());
                }
            }
            Err(error) => {
                println!("{id}: render the release: {error:#}");
                failed.push(id.clone());
            }
        }
    }
    if !failed.is_empty() {
        failed.sort_unstable();
        let failed = failed.iter().map(PackageName::as_str).collect::<Vec<_>>();
        bail!("failed: {}", failed.join(", "));
    }
    Ok(())
}

/// The manifest that publication would write, with placeholder registry digests.
pub(crate) fn published_manifest(output: &Path) -> Result<PackageManifest> {
    let mut verified = wamn_publish::package_artifact::verify_package(output)?;
    let base_packages = verified
        .manifest
        .base_dependencies
        .iter()
        .map(|(alias, base)| (alias.clone(), package_digest(&base.package)))
        .collect();
    let components = verified
        .components
        .into_iter()
        .map(|component| {
            let descriptor = component.descriptor;
            let image_digest =
                bytes_sha256(format!("image {}", descriptor.component_digest).as_bytes());
            (
                descriptor.component.clone(),
                PublishedComponent {
                    descriptor,
                    image_digest,
                },
            )
        })
        .collect();
    let deploy = verified
        .manifest
        .deploy
        .as_mut()
        .context("the build has no deploy section")?;
    deploy.base_packages = base_packages;
    deploy.components = components;
    // Read the published bytes back, as the deploy reads a package artifact.
    let bytes = wamn_digest::canonical_json_bytes(&serde_json::to_value(&verified.manifest)?);
    PackageManifest::from_slice(&bytes).context("read the published manifest")
}

/// A placeholder for the registry's package artifact digest.
pub(crate) fn package_digest(id: &PackageName) -> Digest {
    bytes_sha256(format!("package {id}").as_bytes())
}

/// The stamped application chart leaf, with placeholder platform images.
fn stamp_chart(root: &Path, out: &Path) -> Result<std::path::PathBuf> {
    let zero = "0".repeat(64);
    let set = wamn_platform_install::release_chart::ImageSet {
        host: format!("registry.example.com/wamn/wamn-host:0.1.0@sha256:{zero}"),
        ingress: format!("registry.example.com/wamn/ingress@sha256:{zero}"),
        workflow: format!("registry.example.com/wamn/workflow@sha256:{zero}"),
    };
    let stamped = wamn_kubernetes::release_chart::stamp(
        &root.join("deploy/charts/application"),
        &set,
        out,
        &wamn_oci::platform_install::PlatformRegistry,
    )?;
    wamn_kubernetes::release_chart::bundle::prepare_payload(root, &stamped)?;
    Ok(stamped.join("platform/application"))
}

/// The environment that deploys package `id` as one application.
fn environment(id: &PackageName) -> Result<EnvironmentDocument> {
    let document = serde_json::json!({
        "org": ORG, "project": "wms", "env": "dev", "target": "cloud",
        "revocation_seconds": 60,
        "applications": {id.as_str(): {
            "label": "app",
            "client": {"package": id, "web": format!("sha256:{}", "0".repeat(64))},
        }},
    });
    EnvironmentDocument::from_json(&serde_json::to_vec(&document)?)
}

/// Render the release of package `id` and its bases, and return the size of
/// its record: base64 of the gzip of the release JSON, as Helm stores it.
async fn record_bytes(
    published: &BTreeMap<PackageName, PackageManifest>,
    id: &PackageName,
    chart: &Path,
    directory: &Path,
) -> Result<usize> {
    let mut manifests = BTreeMap::new();
    let mut pending = vec![id];
    while let Some(id) = pending.pop() {
        if manifests.contains_key(id) {
            continue;
        }
        let manifest = published
            .get(id)
            .with_context(|| format!("base {id} is not a verified package under apps/"))?;
        pending.extend(
            manifest
                .base_dependencies
                .values()
                .map(|base| &base.package),
        );
        manifests.insert(id.clone(), manifest.clone());
    }
    let document = environment(id)?;
    let (application, _) = document
        .applications
        .first_key_value()
        .context("the environment has one application")?;
    let settings = &document.settings;
    let (values, _) = wamn_environment::environment::write::release_values(
        &document,
        &"wamn.example.com".parse()?,
        application,
        &manifests,
        &package_digest(id),
        &[],
        "registry.example.com/wamn/integrations",
        "registry.example.com/wamn/components",
    )?;
    let path = directory.join("application.yaml");
    std::fs::write(&path, serde_yaml::to_string(&values)?)?;
    let release = wamn_environment::release_chart::release_name(
        &settings.org,
        &settings.project,
        &settings.env,
        application,
    )?;
    let namespace = wamn_kubernetes::helm::org_release_namespace(ORG);
    let rendered = wamn_kubernetes::helm::run(&[
        OsStr::new("install"),
        OsStr::new(&release),
        chart.as_os_str(),
        OsStr::new("--namespace"),
        OsStr::new(namespace.as_ref()),
        OsStr::new("--skip-crds"),
        OsStr::new("--values"),
        path.as_os_str(),
        OsStr::new("--dry-run=client"),
        OsStr::new("--output"),
        OsStr::new("json"),
    ])
    .await?;
    let release: serde_json::Value =
        serde_json::from_slice(&rendered).context("read the Helm release JSON")?;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(&serde_json::to_vec(&release)?)?;
    Ok(base64::engine::general_purpose::STANDARD
        .encode(encoder.finish()?)
        .len())
}
