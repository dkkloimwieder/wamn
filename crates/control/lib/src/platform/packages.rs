//! Publish the declared packages and install them through cluster Jobs.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde_json::json;
use wamn_package_artifact::{PackageRegistry, Repository};
use wamn_schema_generator::PackageManifest;

use crate::delivery::environment::EnvironmentFile;
use crate::environment::document::EnvironmentDocument;
use crate::package_artifact::{PushPackageRequest, push_package};
use crate::{kubectl, release_chart::Target};

use super::document::ClusterDocument;
use super::install::{SecretData, administrator, runner_image, secret_text};
use super::job::{self, JobRequest, JobSecret};

pub(super) struct Package {
    pub root: PathBuf,
    pub manifest: PackageManifest,
}

pub(super) fn read(repository: &Path, environment: &Path) -> anyhow::Result<Vec<Package>> {
    let path = environment.with_extension("json");
    let file: EnvironmentFile = serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
    )?;
    file.packages
        .into_iter()
        .map(|root| {
            let root = repository.join(root);
            let path = wamn_schema_generator::package_manifest_path(&root);
            let manifest = PackageManifest::from_slice(
                &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
            )?;
            Ok(Package { root, manifest })
        })
        .collect()
}

pub(super) fn registry(document: &ClusterDocument) -> anyhow::Result<PackageRegistry> {
    Ok(PackageRegistry {
        artifact_base: format!("{}/packages", document.registry),
        registry_auth_file: std::env::var_os("WAMN_REGISTRY_AUTH_FILE")
            .map(PathBuf::from)
            .context("set WAMN_REGISTRY_AUTH_FILE for package and release publication")?,
        insecure_registry: false,
        oci_ca_paths: std::env::var_os("WAMN_OCI_CA_PATHS")
            .map(|paths| std::env::split_paths(&paths).collect())
            .unwrap_or_default(),
    })
}

pub(super) fn registry_inputs(
    request: &mut JobRequest,
    registry: &PackageRegistry,
) -> anyhow::Result<()> {
    request.environment.insert(
        "WAMN_REGISTRY_AUTH_FILE".into(),
        "/inputs/registry.json".into(),
    );
    request.files.insert(
        "registry.json".into(),
        std::fs::read_to_string(&registry.registry_auth_file)
            .context("read registry credentials")?,
    );
    let mut paths = Vec::new();
    for (index, path) in registry.oci_ca_paths.iter().enumerate() {
        let name = format!("registry-ca-{index}.crt");
        paths.push(format!("/inputs/{name}"));
        request.files.insert(
            name,
            std::fs::read_to_string(path).context("read registry CA")?,
        );
    }
    let paths = paths.join(":");
    request
        .environment
        .insert("WAMN_OCI_CA_PATHS".into(), paths.clone());
    request
        .environment
        .insert("WASH_OCI_CA_PATHS".into(), paths.replace(':', ","));
    Ok(())
}

pub(super) async fn credentials(
    target: &Target,
    environment: &EnvironmentDocument,
) -> anyhow::Result<[JobSecret; 2]> {
    let secret: SecretData = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "secret",
        "wamn-pg-superuser",
    )
    .await?
    .context("missing Secret platform/wamn-pg-superuser")?;
    let admin = administrator(&secret)?;
    let name = wamn_control_provision::workload_secret_name(
        wamn_control_provision::WorkloadRoleFamily::App,
        &environment.org,
        &environment.project,
        &environment.env,
    );
    let secret = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "hosts",
        "secret",
        &name,
    )
    .await?
    .context("missing environment application Secret")?;
    let application = url::Url::parse(&secret_text(&secret, "url")?)
        .map_err(|_| anyhow::anyhow!("parse application database URL"))?;
    let mut database = url::Url::parse(&admin.value)
        .map_err(|_| anyhow::anyhow!("parse administrator database URL"))?;
    database.set_path(application.path());
    Ok([
        admin,
        JobSecret {
            name: "platform-package-admin".into(),
            key: "url".into(),
            value: database.into(),
            environment: "WAMN_PG_ADMIN_URL".into(),
        },
    ])
}

pub(super) async fn apply(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
    environments: &Path,
    directory: &Path,
) -> anyhow::Result<bool> {
    let registry = registry(document)?;
    let image = runner_image(directory)?;
    let mut changed = false;
    for (path, environment) in super::broker::documents(environments)? {
        let tenant = format!(
            "{}--{}--{}",
            environment.org, environment.project, environment.env
        );
        let secrets = credentials(target, &environment).await?;
        let mut tags = Vec::new();
        let mut artifacts = Vec::new();
        let mut replicas = Vec::new();
        for package in read(repository, &path)? {
            let identity = &package.manifest.package;
            let tag = format!("{}-{}", identity.id, identity.version);
            let artifact = Repository::open(&registry, &tag)?.manifest().await?;
            let digest = match artifact {
                Some((_, digest)) => digest,
                None => {
                    let pushed = push_package(&PushPackageRequest {
                        package: package.root.clone(),
                        registry: registry.clone(),
                        component_artifact_base: format!("{}/components", document.registry),
                    })
                    .await?;
                    changed = true;
                    pushed.digest
                }
            };
            tags.push(tag.clone());
            artifacts.push(digest.clone());
            let mut request = JobRequest {
                name: format!(
                    "platform-package-{}",
                    &job::input_digest(&json!([tenant, tag]))[..24]
                ),
                image: image.clone(),
                arguments: vec![
                    "apply-package".into(),
                    "--package-artifact".into(),
                    tag,
                    "--artifact-base".into(),
                    registry.artifact_base.clone(),
                    "--tenant".into(),
                    tenant.clone(),
                ],
                ..JobRequest::default()
            };
            registry_inputs(&mut request, &registry)?;
            changed |= job::run(
                target,
                directory,
                &request,
                &json!({"artifact": digest}),
                &secrets,
            )
            .await?;
            if identity.id == "wamn_wms" {
                let mut request = JobRequest {
                    name: format!("platform-replica-{tenant}"),
                    image: image.clone(),
                    arguments: vec![
                        "reconcile-replica-identity".into(),
                        "--package".into(),
                        "/inputs/package".into(),
                    ],
                    ..JobRequest::default()
                };
                request.files.insert(
                    "package/wamn.json".into(),
                    serde_json::to_string(&package.manifest)?,
                );
                for entry in std::fs::read_dir(package.root.join("migrations"))? {
                    let entry = entry?;
                    request.files.insert(
                        format!("package/migrations/{}", entry.file_name().to_string_lossy()),
                        std::fs::read_to_string(entry.path())?,
                    );
                }
                replicas.push((request, digest));
            }
        }
        if !tags.is_empty() {
            let mut request = JobRequest {
                name: format!("platform-package-access-{tenant}"),
                image: image.clone(),
                arguments: vec![
                    "reconcile-package-data-access".into(),
                    "--artifact-base".into(),
                    registry.artifact_base.clone(),
                    "--tenant".into(),
                    tenant,
                ],
                ..JobRequest::default()
            };
            for tag in &tags {
                request
                    .arguments
                    .extend(["--package-artifact".into(), tag.clone()]);
            }
            registry_inputs(&mut request, &registry)?;
            changed |= job::run(
                target,
                directory,
                &request,
                &json!({"artifacts": artifacts}),
                &secrets,
            )
            .await?;
        }
        for (request, digest) in replicas {
            changed |= job::run(
                target,
                directory,
                &request,
                &json!({"artifact": digest}),
                &secrets,
            )
            .await?;
        }
    }
    Ok(changed)
}
