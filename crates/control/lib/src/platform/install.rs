//! Install the platform through runbook step 4.21 using kubectl, Helm, and HTTPS.

use std::path::Path;
use std::process::Stdio;
use std::time::Instant;

use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::rand::SecureRandom as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::kubectl;
use crate::release_chart::Target;

use super::document::ClusterDocument;
use super::job::{self, JobRequest, JobSecret};
use super::manifests::{already_applied, objects, operator_values};
use super::steps::{InstallStep, install_steps};
use super::{build, lock};

// The runtime operator version pinned by docs/operations/gcp.md section 4.4.
const OPERATOR_VERSION: &str = "2.11.0";

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
    let _lock = lock::take(work_directory)?;
    let kubeconfig = work_directory.join(format!("{}.kubeconfig", document.cluster_name));
    let context = document.provider.kubeconfig(document, &kubeconfig).await?;
    let target = Target {
        kubeconfig,
        context,
        namespace: "platform".into(),
    };
    let mut published = None;
    for step in applied_steps(document, platform_version) {
        let started = Instant::now();
        progress(&format!("{} {}: starting", step.number, step.title))?;
        let changed = match step.number {
            "4.3" => certificates(&target, document, repository, &step).await?,
            "4.4" => runtime(&target, document, repository, work_directory, &step).await?,
            "4.4.1" => collector(&target, document, repository, &step).await?,
            "4.5" => postgres(&target, document, repository, &step).await?,
            "4.6" => {
                build::apply(
                    document,
                    platform_version,
                    repository,
                    environments,
                    work_directory,
                )
                .await?
            }
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
            "4.13" => {
                super::packages::apply(&target, repository, environments, work_directory, registry)
                    .await?
            }
            "4.14" => {
                let result = super::releases::apply(
                    &target,
                    repository,
                    environments,
                    work_directory,
                    registry,
                )
                .await?;
                let changed = result.changed;
                published = Some(result);
                changed
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
    if let Some(published) = published {
        for (path, _, digest) in &published.releases {
            progress(&format!("{}: release = \"{digest}\"", path.display()))?;
        }
        if published
            .releases
            .iter()
            .any(|(_, environment, digest)| unrecorded(&environment.release, digest))
        {
            progress(
                "Commit the release lines in the environment files, then rerun platform apply for step 4.17.",
            )?;
            return Ok(());
        }
    }
    progress("4.15 Qualification: skipped: qualify-release on the build machine")?;
    progress("4.16 Record the releases: skipped: owner input")?;
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

/// Whether 4.16 must stop for this environment: its committed release is
/// `none` or a digest other than the one 4.14 published, so 4.17 would apply
/// an old release (wamn-ky3mq).
fn unrecorded(declared: &crate::environment::document::DeclaredRelease, published: &str) -> bool {
    *declared != crate::environment::document::DeclaredRelease::Digest(published.to_owned())
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
                "4.3"
                    | "4.4"
                    | "4.4.1"
                    | "4.5"
                    | "4.6"
                    | "4.7"
                    | "4.8"
                    | "4.9"
                    | "4.11"
                    | "4.12"
                    | "4.13"
                    | "4.14"
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

fn provisioning_jobs(document: &ClusterDocument, image: &str) -> [JobRequest; 3] {
    let domain = document
        .route_hosts
        .control
        .split_once('.')
        .map_or(document.route_hosts.control.as_str(), |(_, domain)| domain);
    [
        (
            "platform-system",
            vec!["provision-system", "--platform-domain", domain],
            false,
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
    let secrets = [administrator(&secret)?];
    // Each Job creates its records once. Its arguments are its only input, so
    // a new version, image or registry does not rerun it (wamn-xjna8).
    let inputs = json!({});
    let mut changed = false;
    for request in provisioning_jobs(document, &image) {
        changed |= job::run(target, directory, &request, &inputs, &secrets).await?;
    }
    for path in [
        "deploy/gcp/identity-certificate.yaml",
        "deploy/gcp/identity-operator-ca.yaml",
    ] {
        for object in objects(repository, path, document)? {
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
    for configmap in identity_ca(&secret)? {
        changed |= apply_object(target, &serde_json::to_value(configmap)?).await?;
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

async fn certificates(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
    step: &InstallStep,
) -> anyhow::Result<bool> {
    let mut changed = apply_file(
        target,
        document,
        repository,
        "deploy/infra/cert-manager.yaml",
    )
    .await?;
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
    let edge = objects(repository, "deploy/gcp/letsencrypt.yaml", document)?;
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
    for namespace in ["platform", "identity", "hosts"] {
        changed |= apply_object(
            target,
            &json!({
                "apiVersion": "v1", "kind": "Namespace", "metadata": {"name": namespace}
            }),
        )
        .await?;
    }
    let values = operator_values(repository, document)?;
    let values_path = work_directory.join("values-operator.yaml");
    write_operator_values(&values_path, &values)?;
    changed |= install_operator(target, &values_path, &values).await?;
    changed |= apply_file(
        target,
        document,
        repository,
        "deploy/platform/runtime-operator-events-rbac.example.yaml",
    )
    .await?;
    let ca: Value = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "secret",
        "wasmcloud-ca",
    )
    .await?
    .context("missing Secret platform/wasmcloud-ca after operator install")?;
    let desired = ca_copy(&ca)?;
    let live: Option<Value> = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "cert-manager",
        "secret",
        "wasmcloud-ca",
    )
    .await?;
    if !live.as_ref().is_some_and(|live| ca_matches(&desired, live)) {
        kubectl::apply(
            &target.kubeconfig,
            &target.context,
            "cert-manager",
            &desired,
        )
        .await?;
        changed = true;
    }
    changed |= apply_file(
        target,
        document,
        repository,
        "deploy/infra/wasmcloud-ca-issuer.yaml",
    )
    .await?;
    ready(
        target,
        step,
        "cert-manager",
        "clusterissuer",
        "wasmcloud-ca",
        "60s",
    )
    .await?;
    changed |= apply_file(
        target,
        document,
        repository,
        "deploy/platform/host-environment-certs.example.yaml",
    )
    .await?;
    for name in ["wasmcloud-runtime-tls", "wasmcloud-data-tls"] {
        ready(target, step, "hosts", "certificate", name, "120s").await?;
    }
    Ok(changed)
}

async fn collector(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
    step: &InstallStep,
) -> anyhow::Result<bool> {
    let mut changed = apply_file(
        target,
        document,
        repository,
        "deploy/platform/otel-collector.yaml",
    )
    .await?;
    changed |= apply_file(
        target,
        document,
        repository,
        "deploy/gcp/otel-collector-values.yaml",
    )
    .await?;
    rollout(target, step, "platform", "deploy/otel-collector", "300s").await?;
    Ok(changed)
}

async fn postgres(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
    step: &InstallStep,
) -> anyhow::Result<bool> {
    let mut changed = apply_file(
        target,
        document,
        repository,
        "deploy/infra/cnpg-operator.yaml",
    )
    .await?;
    rollout(
        target,
        step,
        "cnpg-system",
        "deploy/cnpg-controller-manager",
        "180s",
    )
    .await?;
    changed |= apply_file(target, document, repository, "deploy/gcp/cnpg-cluster.yaml").await?;
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
    let authorization: Option<Value> = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "secret",
        "evt-nats-authorization",
    )
    .await?;
    let mut changed = false;
    if let Some(secret) = broker_secret(authorization.as_ref())? {
        kubectl::apply(&target.kubeconfig, &target.context, "platform", &secret).await?;
        changed = true;
    }
    changed |= super::broker::apply(target, repository, environments, &document.org).await?;
    let authorization: Value = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "secret",
        "evt-nats-authorization",
    )
    .await?
    .context("missing Secret platform/evt-nats-authorization")?;
    for mut object in objects(repository, "deploy/gcp/nats-jetstream.yaml", document)? {
        if object["kind"] == "StatefulSet" {
            object["spec"]["template"]["metadata"]["annotations"] = json!({
                "wamn.dev/event-authorization": job::input_digest(&authorization["data"])
            });
        }
        changed |= apply_object(target, &object).await?;
    }
    rollout(target, step, "platform", "statefulset/evt-nats", "300s").await?;
    Ok(changed)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BrokerSecret {
    api_version: &'static str,
    kind: &'static str,
    metadata: SecretMetadata,
    #[serde(rename = "type")]
    secret_type: &'static str,
    string_data: std::collections::BTreeMap<&'static str, String>,
}

#[derive(Serialize)]
struct SecretMetadata {
    name: &'static str,
    namespace: &'static str,
}

fn broker_secret(authorization: Option<&Value>) -> anyhow::Result<Option<BrokerSecret>> {
    if authorization.is_some() {
        return Ok(None);
    }
    let mut random = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| anyhow::anyhow!("generate broker password"))?;
    Ok(Some(render_broker_secret(&hex::encode(random))))
}

fn render_broker_secret(password: &str) -> BrokerSecret {
    // An empty users list lets nats-server accept anonymous clients, so the
    // authorization holds one user that may do nothing until the environment
    // step rewrites it. Nothing keeps the password.
    // It stays plain text: it grants nothing, so a hash would protect nothing.
    let authorization = json!({
        "users": [{
            "user": "platform-seal",
            "password": password,
            "permissions": {
                "publish": {"deny": [">"]},
                "subscribe": {"deny": [">"]}
            }
        }]
    });
    BrokerSecret {
        api_version: "v1",
        kind: "Secret",
        metadata: SecretMetadata {
            name: "evt-nats-authorization",
            namespace: "platform",
        },
        secret_type: "Opaque",
        string_data: [(
            "authorization.conf",
            format!("authorization: {authorization}\n"),
        )]
        .into(),
    }
}

async fn apply_file(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
    path: &str,
) -> anyhow::Result<bool> {
    let mut changed = false;
    for object in objects(repository, path, document)? {
        changed |= apply_object(target, &object)
            .await
            .with_context(|| format!("apply manifest {path}"))?;
    }
    Ok(changed)
}

pub(super) async fn apply_object(target: &Target, object: &Value) -> anyhow::Result<bool> {
    let namespace = object["metadata"]["namespace"]
        .as_str()
        .unwrap_or(&target.namespace);
    let name = object["metadata"]["name"]
        .as_str()
        .context("manifest object has no metadata.name")?;
    let kind = object["kind"]
        .as_str()
        .context("manifest object has no kind")?;
    let api_version = object["apiVersion"]
        .as_str()
        .context("manifest object has no apiVersion")?;
    let resource = api_version
        .split_once('/')
        .map_or_else(|| kind.to_owned(), |(group, _)| format!("{kind}.{group}"));
    let live: Option<Value> = kubectl::get(
        &target.kubeconfig,
        &target.context,
        namespace,
        &resource,
        name,
    )
    .await?;
    if live
        .as_ref()
        .is_some_and(|live| already_applied(object, live))
    {
        return Ok(false);
    }
    kubectl::apply(&target.kubeconfig, &target.context, namespace, object)
        .await
        .with_context(|| format!("apply {resource} {namespace}/{name}"))?;
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

fn ca_copy(source: &Value) -> anyhow::Result<Value> {
    Ok(json!({
        "apiVersion": "v1", "kind": "Secret",
        "metadata": {"name": "wasmcloud-ca", "namespace": "cert-manager"},
        "data": source.get("data").context("Secret platform/wasmcloud-ca has no data")?,
        "type": source.get("type").context("Secret platform/wasmcloud-ca has no type")?
    }))
}

fn ca_matches(desired: &Value, live: &Value) -> bool {
    desired.get("data") == live.get("data") && desired.get("type") == live.get("type")
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

fn skip_operator(releases: &Value, live_values: Option<&Value>, desired: &Value) -> bool {
    releases.as_array().is_some_and(|releases| {
        releases.iter().any(|release| {
            release["name"] == "wamn"
                && release["chart"] == format!("runtime-operator-{OPERATOR_VERSION}")
        })
    }) && live_values == Some(desired)
}

async fn install_operator(
    target: &Target,
    values_path: &Path,
    values: &Value,
) -> anyhow::Result<bool> {
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
    if skip_operator(&releases, live_values.as_ref(), values) {
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
            "oci://ghcr.io/wasmcloud/charts/runtime-operator".as_ref(),
            "--version".as_ref(),
            OPERATOR_VERSION.as_ref(),
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

pub(super) async fn helm(
    target: &Target,
    arguments: &[&std::ffi::OsStr],
) -> anyhow::Result<Vec<u8>> {
    let output = Command::new("helm")
        .arg("--kubeconfig")
        .arg(&target.kubeconfig)
        .args(["--kube-context", &target.context])
        .args(arguments)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("start helm")?;
    ensure!(
        output.status.success(),
        "helm {} exited {}: {}",
        arguments
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_none_or_stale_committed_release_stops_at_4_16() {
        use crate::environment::document::DeclaredRelease;
        let published = format!("sha256:{}", "a".repeat(64));
        assert!(unrecorded(&DeclaredRelease::None, &published));
        assert!(unrecorded(
            &DeclaredRelease::Digest(format!("sha256:{}", "b".repeat(64))),
            &published
        ));
        assert!(!unrecorded(
            &DeclaredRelease::Digest(published.clone()),
            &published
        ));
    }
    use crate::platform::document::Provider;

    #[test]
    fn broker_secret_renders_one_user_without_allows() {
        let secret = serde_json::to_value(broker_secret(None).unwrap().unwrap()).unwrap();
        assert_eq!(secret["apiVersion"], "v1");
        assert_eq!(secret["kind"], "Secret");
        assert_eq!(secret["type"], "Opaque");
        assert_eq!(
            secret["metadata"],
            json!({"name": "evt-nats-authorization", "namespace": "platform"})
        );
        let data = secret["stringData"].as_object().unwrap();
        assert_eq!(data.len(), 1);
        let text = data["authorization.conf"].as_str().unwrap();
        let authorization: Value =
            serde_json::from_str(text.strip_prefix("authorization: ").unwrap()).unwrap();
        let users = authorization["users"].as_array().unwrap();
        assert_eq!(users.len(), 1);
        let password = users[0]["password"].as_str().unwrap();
        assert_eq!(hex::decode(password).unwrap().len(), 32);
        assert_eq!(
            users[0],
            json!({
                "user": "platform-seal", "password": password,
                "permissions": {
                    "publish": {"deny": [">"]},
                    "subscribe": {"deny": [">"]}
                }
            })
        );
        // The password appears only inside the authorization file itself.
        assert_eq!(secret.to_string().matches(password).count(), 1);
    }

    #[test]
    fn existing_authorization_skips_the_secret_without_reading_its_users() {
        for existing in [
            json!({}),
            json!({"data": {"authorization.conf": "e3-users"}}),
        ] {
            assert!(broker_secret(Some(&existing)).unwrap().is_none());
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
                "4.3", "4.4", "4.4.1", "4.5", "4.6", "4.7", "4.8", "4.9", "4.11", "4.12", "4.13",
                "4.14"
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
                ["provision-system", "--platform-domain", "example.com"]
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
            assert!(!jobs[0].emits_secret && !jobs[1].emits_secret && jobs[2].emits_secret);
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
        let releases = json!([{"name": "wamn", "chart": "runtime-operator-2.11.0"}]);
        let desired = json!({"operator": {"watchNamespaces": ["hosts"]}});
        assert!(skip_operator(&releases, Some(&desired), &desired));
        assert!(!skip_operator(&json!([]), Some(&desired), &desired));
        assert!(!skip_operator(&releases, None, &desired));
        assert!(!skip_operator(
            &releases,
            Some(&json!({"operator": {}})),
            &desired
        ));
        assert!(!skip_operator(
            &json!([{"name": "wamn", "chart": "runtime-operator-2.10.2"}]),
            Some(&desired),
            &desired
        ));
        assert!(!skip_operator(
            &json!([{"name": "other", "chart": "runtime-operator-2.11.0"}]),
            Some(&desired),
            &desired
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
            .filter(|step| matches!(step.number, "4.3" | "4.4" | "4.4.1" | "4.5"))
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
    fn ca_copy_compares_only_data_and_type() {
        let source = json!({
            "metadata": {"namespace": "platform", "uid": "source", "ownerReferences": [{}]},
            "data": {"tls.crt": "cert", "tls.key": "key"}, "type": "kubernetes.io/tls"
        });
        let desired = ca_copy(&source).unwrap();
        assert_eq!(
            desired["metadata"],
            json!({"name": "wasmcloud-ca", "namespace": "cert-manager"})
        );
        assert!(ca_matches(&desired, &source));
        let mut changed = source.clone();
        changed["data"]["tls.crt"] = "rotated".into();
        assert!(!ca_matches(&desired, &changed));
        changed = source;
        changed["type"] = "Opaque".into();
        assert!(!ca_matches(&desired, &changed));
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
