//! Install the platform through runbook step 4.21 using kubectl, Helm, and HTTPS.

use std::path::Path;
use std::time::Instant;

use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::kubectl;
use wamn_release_control::release_chart::Target;

use super::document::ClusterDocument;
use super::job::{self, JobRequest, JobSecret};
use super::lock;
use super::manifests::objects;
pub use super::resources::{
    cert_manager_objects, helm_template, namespace_object, operator_entry, postgres_cluster_inputs,
    postgres_operator_objects, runtime_events,
};
pub(super) use super::resources::{
    helm, identity_certificate_objects, nats_objects, operator_objects, postgres_cluster_objects,
    runtime_certificates, runtime_namespaces,
};
use super::steps::{InstallStep, install_steps};

/// Apply steps 4.3 through 4.21 under the install lock, excluding owner steps.
///
/// `progress` receives one line at the start and end of each step.
///
/// # Errors
///
/// Returns the first file, process, readiness, or progress-output error.
pub async fn apply(
    document: &ClusterDocument,
    platform_version: &str,
    repository: &Path,
    environments: &Path,
    work_directory: &Path,
    registry: &wamn_package_artifact::PackageRegistry,
    mut progress: impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    require_artifacts(document, platform_version, environments, work_directory)?;
    let _lock = lock::take(work_directory)?;
    let kubeconfig = work_directory.join(format!("{}.kubeconfig", document.cluster_name));
    let context = document.provider.kubeconfig(document, &kubeconfig).await?;
    let target = Target {
        kubeconfig,
        context,
        namespace: "platform".into(),
    };
    for step in applied_steps(document, platform_version) {
        let started = Instant::now();
        progress(&format!("{} {}: starting", step.number, step.title))?;
        let changed = match step.number {
            "4.3" => certificates(&target, document, repository, &step).await?,
            "4.4" => runtime(&target, document, repository, work_directory, &step).await?,
            "4.4.1" => collector(&target, document, repository, &step).await?,
            "4.5" => postgres(&target, document, repository, &step).await?,
            "4.7" => provisioning(&target, document, repository, work_directory, &step).await?,
            "4.8" => broker(&target, document, repository, environments, &step).await?,
            "4.9" => platform_inputs(&target, document, repository).await?,
            "4.11" => {
                super::environments::apply(&target, document, environments, work_directory, None)
                    .await?
            }
            "4.12" => {
                super::identity::apply(&target, document, repository, environments, work_directory)
                    .await?
            }
            _ => unreachable!("the install step filter selects only implemented steps"),
        };
        progress(&format!(
            "{} {}: {} {:.1}s",
            step.number,
            step.title,
            if changed { "done" } else { "skipped" },
            started.elapsed().as_secs_f64()
        ))?;
    }
    let started = Instant::now();
    progress("4.16.1 Connections: starting")?;
    let changed =
        super::environments::connections(&target, document, environments, work_directory).await?;
    progress(&format!(
        "4.16.1 Connections: {} {:.1}s",
        if changed { "done" } else { "skipped" },
        started.elapsed().as_secs_f64()
    ))?;
    let started = Instant::now();
    progress("4.17 Apply with the release: starting")?;
    let changed = super::environments::apply(
        &target,
        document,
        environments,
        work_directory,
        Some(registry),
    )
    .await?;
    progress(&format!(
        "4.17 Apply with the release: {} {:.1}s",
        if changed { "done" } else { "skipped" },
        started.elapsed().as_secs_f64()
    ))?;
    let started = Instant::now();
    progress("4.18 CDC readers: starting")?;
    let changed = super::cdc::apply(&target, environments, work_directory, &mut progress).await?;
    progress(&format!(
        "4.18 CDC readers: {} {:.1}s",
        if changed { "done" } else { "skipped" },
        started.elapsed().as_secs_f64()
    ))?;
    super::web::apply(
        &target,
        document,
        environments,
        work_directory,
        registry,
        &mut progress,
    )
    .await?;
    let started = Instant::now();
    progress("4.19.1 Control credentials: starting")?;
    let changed = super::control::credentials(&target, document, work_directory).await?;
    progress(&format!(
        "4.19.1 Control credentials: {} {:.1}s",
        if changed { "done" } else { "skipped" },
        started.elapsed().as_secs_f64()
    ))?;
    let started = Instant::now();
    progress("4.19.1 Control host: starting")?;
    let changed =
        super::control::release(&target, document, repository, work_directory, registry).await?;
    progress(&format!(
        "4.19.1 Control host: {} {:.1}s",
        if changed { "done" } else { "skipped" },
        started.elapsed().as_secs_f64()
    ))?;
    super::web::platform_ui(&target, document, repository, work_directory, &mut progress).await?;
    let started = Instant::now();
    progress("4.19.1 Owner invite: starting")?;
    let changed = super::invite::apply(&target, document, work_directory).await?;
    progress(&format!(
        "4.19.1 Owner invite: {} {:.1}s",
        if changed { "done" } else { "skipped" },
        started.elapsed().as_secs_f64()
    ))?;
    let started = Instant::now();
    progress("4.20 Rust edge: starting")?;
    let changed = super::edge::apply(&target, document, environments, work_directory).await?;
    progress(&format!(
        "4.20 Rust edge: {} {:.1}s",
        if changed { "done" } else { "skipped" },
        started.elapsed().as_secs_f64()
    ))?;
    super::edge::check(
        &target,
        document,
        environments,
        work_directory,
        &mut progress,
    )
    .await?;
    let started = Instant::now();
    progress("4.20 Edge proxy: starting")?;
    let changed = super::edge::proxy(
        &target,
        document,
        environments,
        repository,
        work_directory,
        &mut progress,
    )
    .await?;
    progress(&format!(
        "4.20 Edge proxy: {} {:.1}s",
        if changed { "done" } else { "skipped" },
        started.elapsed().as_secs_f64()
    ))?;
    let started = Instant::now();
    progress("4.21 Check: starting")?;
    let changed = super::check::apply(
        &target,
        document,
        environments,
        work_directory,
        registry,
        &mut progress,
    )
    .await?;
    progress(&format!(
        "4.21 Check: {} {:.1}s",
        if changed { "done" } else { "skipped" },
        started.elapsed().as_secs_f64()
    ))?;
    Ok(())
}

/// Read the existing package registry environment inputs for an install.
///
/// # Errors
///
/// Returns an error when WAMN_REGISTRY_AUTH_FILE is unset.
pub fn registry(
    document: &ClusterDocument,
) -> anyhow::Result<wamn_package_artifact::PackageRegistry> {
    super::packages::registry(document)
}

/// Require published inputs before the installer changes the cluster.
fn require_artifacts(
    document: &ClusterDocument,
    version: &str,
    environments: &Path,
    directory: &Path,
) -> anyhow::Result<()> {
    let chart = directory.join("release-chart");
    ensure!(
        wamn_release_control::release_chart::read_platform_version(&chart.join("Chart.yaml"))?
            == version,
        "the prepared release chart must name platform {version}"
    );
    let values: Value = serde_yaml::from_slice(&std::fs::read(chart.join("values.yaml"))?)?;
    ensure!(
        super::artifact_plan::images_match(&values, &document.registry, version),
        "prepare the published platform {version} chart in {} before platform apply",
        chart.display()
    );
    for (path, environment) in super::broker::documents(environments)? {
        ensure!(
            matches!(
                environment.release,
                crate::environment::document::DeclaredRelease::Digest(_)
            ),
            "{} must select a published, qualified release before platform apply",
            path.display()
        );
    }
    Ok(())
}

fn applied_steps(
    document: &ClusterDocument,
    platform_version: &str,
) -> impl Iterator<Item = InstallStep> {
    install_steps(document, platform_version)
        .into_iter()
        .filter(|step| {
            matches!(
                step.number,
                "4.3" | "4.4" | "4.4.1" | "4.5" | "4.7" | "4.8" | "4.9" | "4.11" | "4.12"
            )
        })
}

pub(super) const DATABASE_HOST: &str = "wamn-pg-rw.platform.svc.cluster.local";

#[derive(Deserialize)]
pub(super) struct SecretData {
    #[serde(default)]
    data: std::collections::BTreeMap<String, String>,
}

pub(super) fn secret_text(secret: &SecretData, key: &str) -> anyhow::Result<String> {
    let encoded = secret
        .data
        .get(key)
        .with_context(|| format!("the Secret has no {key}"))?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| anyhow::anyhow!("the Secret key {key} is not base64"))?;
    String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("the Secret key {key} is not UTF-8"))
}

pub(super) fn administrator(secret: &SecretData) -> anyhow::Result<JobSecret> {
    let mut url = url::Url::parse(&format!(
        "postgresql://postgres@{DATABASE_HOST}/wamn_system"
    ))?;
    url.set_password(Some(&secret_text(secret, "password")?.replace('%', "%25")))
        .map_err(|()| anyhow::anyhow!("encode the system administrator password"))?;
    Ok(JobSecret {
        name: "wamn-system-admin".into(),
        key: "url".into(),
        value: url.into(),
        environment: "WAMN_SYSTEM_ADMIN_URL".into(),
    })
}

pub(super) fn provisioning_jobs(document: &ClusterDocument, image: &str) -> [JobRequest; 3] {
    let domain = document
        .route_hosts
        .control
        .split_once('.')
        .map_or(document.route_hosts.control.as_str(), |(_, domain)| domain);
    [
        (
            "platform-system",
            vec![
                "provision-system",
                "--platform-domain",
                domain,
                "--emit-apply-secret",
                "/out/secret.json",
                "--db-host",
                DATABASE_HOST,
            ],
            true,
        ),
        (
            "platform-org",
            vec![
                "provision-org",
                "--org",
                &document.org,
                "--template",
                "trials",
                "--pool",
                "wamn-pg",
                "--admin",
                &document.owner_email,
            ],
            false,
        ),
        (
            "platform-identity-issuer",
            vec![
                "provision-identity-issuer",
                "--issuer",
                "https://identity.identity.svc.cluster.local",
                "--prepare-generation",
                "a",
                "--namespace",
                "identity",
                "--emit-secret",
                "/out/secret.json",
                "--db-host",
                DATABASE_HOST,
            ],
            true,
        ),
    ]
    .map(|(name, arguments, emits_secret)| JobRequest {
        name: name.into(),
        image: image.into(),
        arguments: arguments.into_iter().map(str::to_owned).collect(),
        calls_kubernetes: false,
        emits_secret,
        once: true,
        ..JobRequest::default()
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CaConfigMap {
    api_version: &'static str,
    kind: &'static str,
    metadata: SecretMetadata,
    data: std::collections::BTreeMap<&'static str, String>,
}

#[derive(Serialize)]
struct SecretMetadata {
    name: &'static str,
    namespace: &'static str,
}

fn identity_ca(secret: &SecretData) -> anyhow::Result<[CaConfigMap; 2]> {
    let ca = secret_text(secret, "ca.crt")?;
    Ok(["hosts", "edge"].map(|namespace| CaConfigMap {
        api_version: "v1",
        kind: "ConfigMap",
        metadata: SecretMetadata {
            name: "identity-ca",
            namespace,
        },
        data: [("ca.crt", ca.clone())].into(),
    }))
}

pub(super) fn collector_objects(
    repository: &Path,
    document: &ClusterDocument,
) -> anyhow::Result<Vec<Value>> {
    let mut rendered = objects(
        repository,
        "deploy/gcp/otel-collector-values.yaml",
        document,
    )?;
    rendered.extend(objects(
        repository,
        "deploy/platform/otel-collector.yaml",
        document,
    )?);
    Ok(rendered)
}

pub(super) fn identity_ca_objects(secret: &SecretData) -> anyhow::Result<Vec<Value>> {
    identity_ca(secret)?
        .into_iter()
        .map(|object| serde_json::to_value(object).map_err(Into::into))
        .collect()
}

async fn provisioning(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
    directory: &Path,
    step: &InstallStep,
) -> anyhow::Result<bool> {
    let image = runner_image(directory)?;
    let secret: SecretData = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "secret",
        "wamn-pg-superuser",
    )
    .await?
    .context("missing Secret platform/wamn-pg-superuser")?;
    // Each Job creates its records once. Its arguments are its only input, so
    // a new version, image or registry does not rerun it (wamn-xjna8).
    let mut changed = false;
    for (request, secrets, inputs) in provisioning_inputs(document, &image, &secret)? {
        changed |= job::run(target, directory, &request, &inputs, &secrets).await?;
    }
    for object in identity_certificate_objects(repository, document)? {
        changed |= apply_object(target, &object).await?;
        if object["kind"] == "Certificate" {
            ready(
                target,
                step,
                "identity",
                "certificate",
                object["metadata"]["name"]
                    .as_str()
                    .context("identity Certificate has no name")?,
                "120s",
            )
            .await?;
        }
    }
    let secret = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "identity",
        "secret",
        "identity-tls",
    )
    .await?
    .context("missing Secret identity/identity-tls")?;
    for configmap in identity_ca_objects(&secret)? {
        changed |= apply_object(target, &configmap).await?;
    }
    Ok(changed)
}

pub(super) fn runner_image(directory: &Path) -> anyhow::Result<String> {
    let path = directory.join("release-chart/values.yaml");
    let values: Value = serde_yaml::from_slice(
        &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
    )?;
    values["platform"]["runner"]["image"]
        .as_str()
        .filter(|image| image.contains("@sha256:"))
        .map(str::to_owned)
        .context("the stamped release chart has no pinned runner image")
}
async fn platform_inputs(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
) -> anyhow::Result<bool> {
    let mut changed = false;
    for object in super::manifests::platform_inputs(repository, document)? {
        changed |= apply_object(target, &object).await?;
    }
    Ok(changed)
}

pub(super) fn edge_certificate_objects(
    repository: &Path,
    document: &ClusterDocument,
) -> anyhow::Result<Vec<Value>> {
    objects(repository, "deploy/gcp/letsencrypt.yaml", document)
}

async fn certificates(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
    step: &InstallStep,
) -> anyhow::Result<bool> {
    let mut changed = false;
    for object in cert_manager_objects(repository, document.provider)? {
        changed |= apply_object(target, &object)
            .await
            .context("apply manifest deploy/infra/cert-manager.yaml")?;
    }
    kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        "cert-manager",
        &[
            "rollout",
            "status",
            "deploy/cert-manager",
            "deploy/cert-manager-webhook",
            "deploy/cert-manager-cainjector",
        ],
        None,
    )
    .await
    .map_err(|error| wait_error(step, error))?;
    let edge = edge_certificate_objects(repository, document)?;
    for object in &edge {
        changed |= apply_object(target, object).await?;
    }
    let issuer = edge
        .iter()
        .find(|object| object["kind"] == "ClusterIssuer")
        .context("edge manifest has no ClusterIssuer")?;
    ready(
        target,
        step,
        "cert-manager",
        "clusterissuer",
        issuer["metadata"]["name"]
            .as_str()
            .context("edge issuer has no name")?,
        "120s",
    )
    .await?;
    ready(target, step, "edge", "certificate", "wamn-edge-tls", "600s").await?;
    Ok(changed)
}

async fn runtime(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
    work_directory: &Path,
    step: &InstallStep,
) -> anyhow::Result<bool> {
    let mut changed = false;
    for namespace in runtime_namespaces() {
        changed |= apply_object(target, &namespace).await?;
    }
    for object in runtime_certificates(repository, document)? {
        changed |= apply_object(target, &object)
            .await
            .context("apply manifest deploy/infra/nats-certificates.yaml")?;
    }
    ready(
        target,
        step,
        "cert-manager",
        "clusterissuer",
        "wasmcloud-ca",
        "120s",
    )
    .await?;
    ready(
        target,
        step,
        "cert-manager",
        "certificate",
        "wasmcloud-ca",
        "120s",
    )
    .await?;
    for (namespace, name) in CHART_SECRETS {
        ready(target, step, namespace, "certificate", name, "120s").await?;
    }
    changed |= nats(target, document, repository, step).await?;
    let operator = operator_objects(repository, document)?;
    let values_path = work_directory.join("values-operator.yaml");
    write_operator_values(&values_path, &operator[0]["values"])?;
    changed |= install_operator(target, &values_path, &operator[0]).await?;
    for object in runtime_events(repository, "hosts", "platform")? {
        changed |= apply_object(target, &object)
            .await
            .context("apply manifest deploy/platform/runtime-operator-events-rbac.example.yaml")?;
    }
    Ok(changed)
}

/// The Secrets that the operator chart mounts by name. cert-manager owns them
/// (docs/plan/alignment.md R3).
const CHART_SECRETS: [(&str, &str); 4] = [
    ("platform", "wasmcloud-nats-tls"),
    ("platform", "wasmcloud-operator-tls"),
    ("hosts", "wasmcloud-runtime-tls"),
    ("hosts", "wasmcloud-data-tls"),
];

async fn collector(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
    step: &InstallStep,
) -> anyhow::Result<bool> {
    let mut changed = false;
    for object in collector_objects(repository, document)? {
        changed |= apply_object(target, &object).await?;
    }
    rollout(target, step, "platform", "deploy/otel-collector", "300s").await?;
    Ok(changed)
}

async fn postgres(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
    step: &InstallStep,
) -> anyhow::Result<bool> {
    let mut changed = false;
    for object in postgres_operator_objects(repository, document.provider)? {
        changed |= apply_object(target, &object)
            .await
            .context("apply manifest deploy/infra/cnpg-operator.yaml")?;
    }
    rollout(
        target,
        step,
        "cnpg-system",
        "deploy/cnpg-controller-manager",
        "180s",
    )
    .await?;
    for object in postgres_cluster_objects(repository, document)? {
        changed |= apply_object(target, &object)
            .await
            .context("apply manifest deploy/infra/cnpg-cluster.yaml")?;
    }
    ready(target, step, "platform", "cluster", "wamn-pg", "600s").await?;
    Ok(changed)
}

async fn broker(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
    environments: &Path,
    step: &InstallStep,
) -> anyhow::Result<bool> {
    let mut changed = super::broker::apply(target, environments, &document.org).await?;
    changed |= nats(target, document, repository, step).await?;
    Ok(changed)
}

/// Apply the one NATS with its authorization file. The first apply writes the
/// platform users, and step 4.8 adds the environments. A changed file reaches
/// the server through its config reloader, never a restart
/// (docs/plan/alignment.md R10).
async fn nats(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
    step: &InstallStep,
) -> anyhow::Result<bool> {
    let mut changed = false;
    let authorization: Option<Value> = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "secret",
        super::broker::AUTHORIZATION,
    )
    .await?;
    for (index, object) in nats_objects(repository, document, authorization.is_some())?
        .into_iter()
        .enumerate()
    {
        if authorization.is_none() && index == 0 {
            kubectl::apply(&target.kubeconfig, &target.context, "platform", &object).await?;
            changed = true;
        } else {
            changed |= apply_object(target, &object).await?;
        }
    }
    rollout(target, step, "platform", "statefulset/wamn-nats", "300s").await?;
    Ok(changed)
}
pub(super) async fn apply_object(target: &Target, object: &Value) -> anyhow::Result<bool> {
    super::resources::apply_object(target, object).await?;
    Ok(true)
}

async fn ready(
    target: &Target,
    step: &InstallStep,
    namespace: &str,
    kind: &str,
    name: &str,
    timeout: &str,
) -> anyhow::Result<()> {
    kubectl::wait(
        &target.kubeconfig,
        &target.context,
        namespace,
        kind,
        name,
        "condition=Ready",
        timeout,
    )
    .await
    .map_err(|error| wait_error(step, error))
}

async fn rollout(
    target: &Target,
    step: &InstallStep,
    namespace: &str,
    resource: &str,
    timeout: &str,
) -> anyhow::Result<()> {
    kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        namespace,
        &[
            "rollout",
            "status",
            resource,
            &format!("--timeout={timeout}"),
        ],
        None,
    )
    .await
    .map(drop)
    .map_err(|error| wait_error(step, error))
}

fn wait_error(step: &InstallStep, error: anyhow::Error) -> anyhow::Error {
    let mut message = format!("{} {}: {error:#}", step.number, step.title);
    for input in &step.owner_inputs {
        message.push_str("\nowner input: ");
        message.push_str(&input.name);
        message.push('\n');
        message.push_str(&input.instructions);
    }
    error.context(message)
}

fn write_operator_values(path: &Path, values: &Value) -> anyhow::Result<()> {
    let bytes = serde_yaml::to_string(values).context("encode operator values")?;
    match std::fs::read(path) {
        Ok(existing) if existing == bytes.as_bytes() => return Ok(()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    }
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

fn skip_operator(
    releases: &Value,
    live_values: Option<&Value>,
    desired: &Value,
    version: &str,
) -> bool {
    releases.as_array().is_some_and(|releases| {
        releases.iter().any(|release| {
            release["name"] == "wamn" && release["chart"] == format!("runtime-operator-{version}")
        })
    }) && live_values == Some(desired)
}

async fn install_operator(
    target: &Target,
    values_path: &Path,
    operator: &Value,
) -> anyhow::Result<bool> {
    let values = &operator["values"];
    let releases: Value = serde_json::from_slice(
        &helm(
            target,
            &[
                "list".as_ref(),
                "-n".as_ref(),
                "platform".as_ref(),
                "-o".as_ref(),
                "json".as_ref(),
            ],
        )
        .await?,
    )
    .context("decode helm list")?;
    let releases_array = releases
        .as_array()
        .context("helm list did not return an array")?;
    let live_values: Option<Value> = if releases_array
        .iter()
        .any(|release| release["name"] == "wamn")
    {
        Some(
            serde_json::from_slice(
                &helm(
                    target,
                    &[
                        "get".as_ref(),
                        "values".as_ref(),
                        "wamn".as_ref(),
                        "-n".as_ref(),
                        "platform".as_ref(),
                        "-o".as_ref(),
                        "json".as_ref(),
                    ],
                )
                .await?,
            )
            .context("decode helm get values")?,
        )
    } else {
        None
    };
    if skip_operator(
        &releases,
        live_values.as_ref(),
        values,
        operator["version"]
            .as_str()
            .context("operator chart version")?,
    ) {
        return Ok(false);
    }
    helm(
        target,
        &[
            "upgrade".as_ref(),
            "--install".as_ref(),
            "--namespace".as_ref(),
            "platform".as_ref(),
            "wamn".as_ref(),
            operator["chart"]
                .as_str()
                .context("operator chart input")?
                .as_ref(),
            "-f".as_ref(),
            values_path.as_os_str(),
            "--wait".as_ref(),
            "--timeout".as_ref(),
            "5m".as_ref(),
        ],
    )
    .await?;
    Ok(true)
}

pub(super) fn provisioning_inputs(
    document: &ClusterDocument,
    image: &str,
    superuser: &SecretData,
) -> anyhow::Result<Vec<job::JobInputs>> {
    let secret = administrator(superuser)?;
    Ok(provisioning_jobs(document, image)
        .into_iter()
        .map(|request| (request, vec![secret.clone()], json!({})))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::super::document::Provider;
    use super::super::manifests::operator_values;
    use super::*;

    #[test]
    fn first_authorization_holds_only_certificate_users() {
        let text = super::super::broker::initial_authorization();
        let authorization: Value =
            serde_json::from_str(text.strip_prefix("authorization: ").unwrap()).unwrap();
        let users = authorization["users"].as_array().unwrap();
        let names: Vec<_> = users
            .iter()
            .map(|user| user["user"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "wasmcloud-operator",
                "wasmcloud-runtime",
                "wasmcloud-data",
                "wamn-identity",
                "wamn-ctl",
                "wamn-cdc-reader",
                "wamn-provisioning"
            ]
        );
        // R4: no client uses a password.
        assert!(!text.contains("password"));
        // A4: the scheduler users reach only the wasmCloud control subjects.
        for user in &users[..2] {
            for direction in ["publish", "subscribe"] {
                assert_eq!(
                    user["permissions"][direction]["allow"],
                    json!(["runtime.host.>", "runtime.operator.>", "_INBOX.>"])
                );
            }
        }
    }

    #[test]
    fn applied_step_list_keeps_the_runbook_order_through_platform_inputs() {
        let document: ClusterDocument = serde_json::from_value(json!({
            "provider": "kind", "project_id": "", "zone": "",
            "cluster_name": "test", "org": "test", "owner_email": "owner@example.com",
            "registry": "localhost:5000", "buckets": {"web": "", "otel": "", "state": ""},
            "route_hosts": {"control": "control.example.com"}, "resend_from": "sender@example.com"
        }))
        .unwrap();
        let numbers: Vec<_> = applied_steps(&document, "test")
            .map(|step| step.number)
            .collect();
        assert_eq!(
            numbers,
            [
                "4.3", "4.4", "4.4.1", "4.5", "4.7", "4.8", "4.9", "4.11", "4.12"
            ]
        );
    }

    #[test]
    fn provision_jobs_use_the_document_and_the_stamped_runner_for_both_providers() {
        for provider in [Provider::Gcp, Provider::Kind] {
            let document: ClusterDocument = serde_json::from_value(json!({
                "provider": provider, "project_id": "", "zone": "",
                "cluster_name": "test", "org": "acme", "owner_email": "owner@example.com",
                "registry": "localhost:5000", "buckets": {"web": "", "otel": "", "state": ""},
                "route_hosts": {"control": "control.example.com"}, "resend_from": "sender@example.com"
            })).unwrap();
            let jobs = provisioning_jobs(&document, "registry/runner:1@sha256:abc");
            assert!(jobs.iter().all(|job| !job.calls_kubernetes && job.image == "registry/runner:1@sha256:abc"));
            assert_eq!(
                jobs[0].arguments,
                [
                    "provision-system",
                    "--platform-domain",
                    "example.com",
                    "--emit-apply-secret",
                    "/out/secret.json",
                    "--db-host",
                    DATABASE_HOST
                ]
            );
            assert_eq!(
                jobs[1].arguments,
                [
                    "provision-org",
                    "--org",
                    "acme",
                    "--template",
                    "trials",
                    "--pool",
                    "wamn-pg",
                    "--admin",
                    "owner@example.com"
                ]
            );
            assert_eq!(
                jobs[2].arguments,
                [
                    "provision-identity-issuer",
                    "--issuer",
                    "https://identity.identity.svc.cluster.local",
                    "--prepare-generation",
                    "a",
                    "--namespace",
                    "identity",
                    "--emit-secret",
                    "/out/secret.json",
                    "--db-host",
                    DATABASE_HOST
                ]
            );
            assert!(jobs[0].emits_secret && !jobs[1].emits_secret && jobs[2].emits_secret);
        }
    }

    #[test]
    fn admin_url_encodes_the_password_and_ca_copies_decode_the_certificate() {
        let secret = SecretData {
            data: [
                ("password".into(), STANDARD.encode("p@ss:/?#%")),
                ("ca.crt".into(), STANDARD.encode("certificate\n")),
            ]
            .into(),
        };
        let admin = administrator(&secret).unwrap();
        assert_eq!(
            admin.value,
            format!("postgresql://postgres:p%40ss%3A%2F%3F%23%25@{DATABASE_HOST}/wamn_system")
        );
        assert_eq!(admin.name, "wamn-system-admin");
        for (configmap, namespace) in identity_ca(&secret)
            .unwrap()
            .into_iter()
            .zip(["hosts", "edge"])
        {
            assert_eq!(
                serde_json::to_value(configmap).unwrap(),
                json!({
                    "apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"identity-ca","namespace":namespace},
                    "data":{"ca.crt":"certificate\n"}
                })
            );
        }
    }

    #[test]
    fn operator_skip_requires_the_named_release_version_and_equal_values() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let operator = operator_entry(
            &repository.join("deploy/platform/release"),
            "platform",
            &json!({}),
        )
        .unwrap();
        let version = operator["version"].as_str().unwrap();
        let chart = format!("runtime-operator-{version}");
        let releases = json!([{"name": "wamn", "chart": chart}]);
        let desired = json!({"operator": {"watchNamespaces": ["hosts"]}});
        assert!(skip_operator(&releases, Some(&desired), &desired, version));
        assert!(!skip_operator(
            &json!([]),
            Some(&desired),
            &desired,
            version
        ));
        assert!(!skip_operator(&releases, None, &desired, version));
        assert!(!skip_operator(
            &releases,
            Some(&json!({"operator": {}})),
            &desired,
            version
        ));
        assert!(!skip_operator(
            &json!([{"name":"wamn","chart":"runtime-operator-other"}]),
            Some(&desired),
            &desired,
            version
        ));
        assert!(!skip_operator(
            &json!([{"name":"other","chart":chart}]),
            Some(&desired),
            &desired,
            version
        ));
    }

    #[test]
    fn failed_wait_appends_each_owner_input_from_the_install_table() {
        let document: ClusterDocument = serde_json::from_value(json!({
            "provider": "gcp", "project_id": "test-project", "zone": "us-central1-a",
            "cluster_name": "test", "org": "test", "owner_email": "owner@example.com",
            "registry": "localhost:5000", "buckets": {"web": "test-web", "otel": "test-otel", "state": "test-state"},
            "route_hosts": {"control": "control.example.com"}, "resend_from": "sender@example.com"
        }))
        .unwrap();
        for step in install_steps(&document, "test")
            .into_iter()
            .filter(|step| matches!(step.number, "4.3" | "4.4" | "4.4.1" | "4.5" | "4.9"))
        {
            let error = wait_error(&step, anyhow::anyhow!("readiness timed out")).to_string();
            assert!(error.starts_with(&format!(
                "{} {}: readiness timed out",
                step.number, step.title
            )));
            for input in &step.owner_inputs {
                assert!(error.contains(&format!(
                    "\nowner input: {}\n{}",
                    input.name, input.instructions
                )));
            }
            if step.owner_inputs.is_empty() {
                assert!(!error.contains("owner input:"));
            }
        }
    }

    #[test]
    fn operator_values_are_written_once_for_each_provider() {
        let directory = std::env::temp_dir().join(format!(
            "wamn-install-values-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("values-operator.yaml");
        for provider in ["gcp", "kind"] {
            let document: ClusterDocument = serde_json::from_value(json!({
                "provider": provider, "project_id": "test-project", "zone": "us-central1-a",
                "cluster_name": "test", "org": "test", "owner_email": "owner@example.com",
                "registry": "localhost:5000", "buckets": {"web": "", "otel": "", "state": ""},
                "route_hosts": {"control": "control.example.com"}, "resend_from": "sender@example.com"
            })).unwrap();
            let values = operator_values(
                Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../..")),
                &document,
            )
            .unwrap();
            write_operator_values(&path, &values).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            let timestamp = std::fs::metadata(&path).unwrap().modified().unwrap();
            write_operator_values(&path, &values).unwrap();
            assert_eq!(
                std::fs::metadata(&path).unwrap().modified().unwrap(),
                timestamp
            );
            assert_eq!(serde_yaml::from_slice::<Value>(&bytes).unwrap(), values);
        }
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
