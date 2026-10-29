//! Publish the declared packages and install them through cluster Jobs.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde_json::json;
use wamn_package_artifact::PackageRegistry;
use wamn_schema_generator::PackageManifest;

use crate::delivery::environment::EnvironmentFile;
use crate::environment::document::EnvironmentDocument;
use crate::package_artifact::{
    PackagePushDisposition, PushPackageRequest, open_package, push_package,
};
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
            wamn_schema_generator::receipt::check_current(&root)?;
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

pub(super) fn artifact_base(
    registry: &PackageRegistry,
    repository: &str,
) -> anyhow::Result<String> {
    let base = registry
        .artifact_base
        .strip_suffix("/packages")
        .context("platform package base must end with /packages")?;
    Ok(format!("{base}/{repository}"))
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
    // An empty CA list sets no variable, because an empty value names one empty path.
    if !paths.is_empty() {
        let paths = paths.join(":");
        request
            .environment
            .insert("WAMN_OCI_CA_PATHS".into(), paths.clone());
        request
            .environment
            .insert("WASH_OCI_CA_PATHS".into(), paths.replace(':', ","));
    }
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
    repository: &Path,
    environments: &Path,
    directory: &Path,
    registry: &PackageRegistry,
) -> anyhow::Result<bool> {
    let image = runner_image(directory)?;
    let auth: SecretData = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "secret",
        "wamn-ctl-auth-nats",
    )
    .await?
    .context("missing Secret platform/wamn-ctl-auth-nats")?;
    let mut changed = false;
    for (path, environment) in super::broker::documents(environments)? {
        let env_key = wamn_control_registry::project_env_key(
            &environment.org,
            &environment.project,
            &environment.env,
        );
        let secrets = credentials(target, &environment).await?;
        let database = url::Url::parse(&secrets[1].value)
            .map_err(|_| anyhow::anyhow!("parse environment database URL"))?;
        let mut tags = Vec::new();
        let mut artifacts = Vec::new();
        let mut replicas = Vec::new();
        for package in read(repository, &path)? {
            let identity = &package.manifest.package;
            let tag = format!("{}-{}", identity.id, identity.version);
            let pushed = push_package(&PushPackageRequest {
                package: package.root.clone(),
                registry: registry.clone(),
                component_artifact_base: artifact_base(registry, "components")?,
            })
            .await?;
            changed |= pushed.disposition == PackagePushDisposition::Pushed;
            let digest = pushed.digest;
            tags.push(tag.clone());
            artifacts.push(digest.clone());
            let mut request =
                package_request(&env_key, tag, &environment, &image, registry, &auth)?;
            registry_inputs(&mut request, registry)?;
            changed |= job::run(
                target,
                directory,
                &request,
                &json!({"artifact": digest, "database": database.path()}),
                &secrets,
            )
            .await?;
            let mut request = replica_request(&env_key, &identity.id, &image);
            let opened = open_package(registry, &identity.id, &identity.version).await?;
            let source = crate::apply_package::read_package_directory(opened.root())?;
            request.files.insert(
                "package/wamn.json".into(),
                String::from_utf8(source.manifest_bytes)?,
            );
            for migration in source.migrations {
                request.files.insert(
                    format!("package/{}", migration.relative_path),
                    String::from_utf8(migration.bytes)?,
                );
            }
            replicas.push((request, digest));
        }
        if !tags.is_empty() {
            let mut request = access_request(&env_key, &environment, &image, registry);
            for tag in &tags {
                request
                    .arguments
                    .extend(["--package-artifact".into(), tag.clone()]);
            }
            registry_inputs(&mut request, registry)?;
            changed |= job::run(
                target,
                directory,
                &request,
                &json!({"artifacts": artifacts, "database": database.path()}),
                &secrets,
            )
            .await?;
        }
        for (request, digest) in replicas {
            changed |= job::run(
                target,
                directory,
                &request,
                &json!({"artifact": digest, "database": database.path()}),
                &secrets,
            )
            .await?;
        }
    }
    Ok(changed)
}

pub(super) fn package_request(
    env_key: &str,
    tag: String,
    environment: &EnvironmentDocument,
    image: &str,
    registry: &PackageRegistry,
    auth: &SecretData,
) -> anyhow::Result<JobRequest> {
    let mut request = JobRequest {
        name: format!(
            "platform-package-{}",
            &job::input_digest(&json!([env_key, tag]))[..24]
        ),
        image: image.into(),
        arguments: vec![
            "apply-package".into(),
            "--package-artifact".into(),
            tag,
            "--artifact-base".into(),
            registry.artifact_base.clone(),
            "--registry-auth-file".into(),
            "/inputs/registry.json".into(),
            "--org".into(),
            environment.org.clone(),
            "--project".into(),
            environment.project.clone(),
            "--environment".into(),
            environment.env.clone(),
        ],
        ..JobRequest::default()
    };
    request.environment.insert(
        "WAMN_COMPONENT_ARTIFACT_BASE".into(),
        artifact_base(registry, "components")?,
    );
    job::auth_nats_inputs(&mut request, Some(auth))?;
    Ok(request)
}

pub(super) fn replica_request(env_key: &str, package: &str, image: &str) -> JobRequest {
    JobRequest {
        name: format!(
            "platform-replica-{}",
            &job::input_digest(&json!([env_key, package]))[..24]
        ),
        image: image.into(),
        arguments: vec![
            "reconcile-replica-identity".into(),
            "--package".into(),
            "/inputs/package".into(),
        ],
        ..JobRequest::default()
    }
}

pub(super) fn access_request(
    env_key: &str,
    environment: &EnvironmentDocument,
    image: &str,
    registry: &PackageRegistry,
) -> JobRequest {
    JobRequest {
        name: format!("platform-package-access-{env_key}"),
        image: image.into(),
        arguments: vec![
            "reconcile-package-data-access".into(),
            "--artifact-base".into(),
            registry.artifact_base.clone(),
            "--registry-auth-file".into(),
            "/inputs/registry.json".into(),
            "--org".into(),
            environment.org.clone(),
            "--project".into(),
            environment.project.clone(),
            "--environment".into(),
            environment.env.clone(),
        ],
        ..JobRequest::default()
    }
}
