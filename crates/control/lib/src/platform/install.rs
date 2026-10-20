//! Install the platform through runbook step 4.8 using kubectl and Helm.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context as _, ensure};
use ring::rand::SecureRandom as _;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::process::Command;

use crate::kubectl;
use crate::release_chart::Target;

use super::document::ClusterDocument;
use super::manifests::{already_applied, objects, operator_values};
use super::steps::{InstallStep, install_steps};
use super::{build, lock};

// The runtime operator version pinned by docs/operations/gcp.md section 4.4.
const OPERATOR_VERSION: &str = "2.10.3";

/// Apply steps 4.3 through 4.6 and the broker part of 4.8 under the install lock.
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
    for step in applied_steps(document, platform_version) {
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
            "4.8" => broker(&target, document, repository, &step).await?,
            _ => unreachable!("the install step filter selects only implemented steps"),
        };
        progress(&format!(
            "{} {}: {}",
            step.number,
            step.title,
            if changed { "done" } else { "skipped" }
        ))?;
    }
    progress("Steps after 4.8 come later.")?;
    Ok(())
}

fn applied_steps(
    document: &ClusterDocument,
    platform_version: &str,
) -> impl Iterator<Item = InstallStep> {
    install_steps(document, platform_version)
        .into_iter()
        .filter(|step| matches!(step.number, "4.3" | "4.4" | "4.4.1" | "4.5" | "4.6" | "4.8"))
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
    if let Some(secrets) = broker_secrets(authorization.as_ref())? {
        // Write the context first so a partial failure cannot leave authorization
        // present without the matching bootstrap password.
        for secret in secrets {
            kubectl::apply(&target.kubeconfig, &target.context, "platform", &secret).await?;
        }
        changed = true;
    }
    changed |= apply_file(
        target,
        document,
        repository,
        "deploy/gcp/nats-jetstream.yaml",
    )
    .await?;
    rollout(target, step, "platform", "statefulset/evt-nats", "300s").await?;
    kubectl::wait(
        &target.kubeconfig,
        &target.context,
        "platform",
        "job",
        "evt-nats-tap-stream",
        "condition=complete",
        "180s",
    )
    .await
    .map_err(|error| wait_error(step, error))?;
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

fn broker_secrets(authorization: Option<&Value>) -> anyhow::Result<Option<[BrokerSecret; 2]>> {
    if authorization.is_some() {
        return Ok(None);
    }
    let mut random = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| anyhow::anyhow!("generate broker password"))?;
    Ok(Some(render_broker_secrets(&hex::encode(random))))
}

fn render_broker_secrets(password: &str) -> [BrokerSecret; 2] {
    // event_broker_files owns the same format in test infrastructure, which is
    // not a production dependency of wamn-control.
    let authorization = json!({
        "users": [{
            "user": "tap-admin",
            "password": password,
            "permissions": {
                "publish": {"allow": [
                    "$JS.API.STREAM.INFO.WAMN_TAP",
                    "$JS.API.STREAM.CREATE.WAMN_TAP",
                    "$JS.API.STREAM.UPDATE.WAMN_TAP",
                    "_INBOX_tap-admin.>"
                ]},
                "subscribe": {"allow": ["_INBOX_tap-admin.>"]}
            }
        }]
    });
    let context = json!({
        "url": "nats://evt-nats.platform.svc.cluster.local:4222",
        "user": "tap-admin",
        "password": password,
        "inbox_prefix": "_INBOX_tap-admin"
    });
    [
        ("evt-nats-bootstrap", "context.json", context.to_string()),
        (
            "evt-nats-authorization",
            "authorization.conf",
            format!("authorization: {authorization}\n"),
        ),
    ]
    .map(|(name, key, value)| BrokerSecret {
        api_version: "v1",
        kind: "Secret",
        metadata: SecretMetadata {
            name,
            namespace: "platform",
        },
        secret_type: "Opaque",
        string_data: [(key, value)].into(),
    })
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

async fn apply_object(target: &Target, object: &Value) -> anyhow::Result<bool> {
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
    kubectl::apply(&target.kubeconfig, &target.context, namespace, object).await?;
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

async fn helm(target: &Target, arguments: &[&std::ffi::OsStr]) -> anyhow::Result<Vec<u8>> {
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
    fn broker_secrets_render_only_the_tap_administrator_and_its_context() {
        let [bootstrap, authorization] = render_broker_secrets("fixed-password");
        let bootstrap = serde_json::to_value(bootstrap).unwrap();
        assert_eq!(
            bootstrap,
            json!({
                "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
                "metadata": {"name": "evt-nats-bootstrap", "namespace": "platform"},
                "stringData": {"context.json": json!({
                    "url": "nats://evt-nats.platform.svc.cluster.local:4222",
                    "user": "tap-admin", "password": "fixed-password",
                    "inbox_prefix": "_INBOX_tap-admin"
                }).to_string()}
            })
        );
        let authorization = serde_json::to_value(authorization).unwrap();
        assert_eq!(authorization["apiVersion"], "v1");
        assert_eq!(authorization["kind"], "Secret");
        assert_eq!(authorization["type"], "Opaque");
        assert_eq!(
            authorization["metadata"],
            json!({"name": "evt-nats-authorization", "namespace": "platform"})
        );
        let data = authorization["stringData"].as_object().unwrap();
        assert_eq!(data.len(), 1);
        let text = data["authorization.conf"].as_str().unwrap();
        let users: Value =
            serde_json::from_str(text.strip_prefix("authorization: ").unwrap()).unwrap();
        assert_eq!(
            users,
            json!({"users": [{
                "user": "tap-admin", "password": "fixed-password",
                "permissions": {
                    "publish": {"allow": [
                        "$JS.API.STREAM.INFO.WAMN_TAP",
                        "$JS.API.STREAM.CREATE.WAMN_TAP",
                        "$JS.API.STREAM.UPDATE.WAMN_TAP",
                        "_INBOX_tap-admin.>"
                    ]},
                    "subscribe": {"allow": ["_INBOX_tap-admin.>"]}
                }
            }]})
        );
    }

    #[test]
    fn existing_authorization_skips_both_secrets_without_reading_its_users() {
        for existing in [
            json!({}),
            json!({"data": {"authorization.conf": "e3-users"}}),
        ] {
            assert!(broker_secrets(Some(&existing)).unwrap().is_none());
        }
        let secrets = broker_secrets(None).unwrap().unwrap();
        let context: Value = serde_json::from_str(&secrets[0].string_data["context.json"]).unwrap();
        let password = context["password"].as_str().unwrap();
        assert_eq!(password.len(), 64);
        assert_eq!(hex::decode(password).unwrap().len(), 32);
    }

    #[test]
    fn applied_step_list_ends_with_the_broker_after_postgres() {
        let document: ClusterDocument = serde_json::from_value(json!({
            "provider": "kind", "project_id": "", "zone": "",
            "cluster_name": "test", "org": "test", "owner_email": "owner@example.com",
            "registry": "localhost:5000", "buckets": {"web": "", "otel": ""},
            "route_hosts": {"control": "control.example.com"}, "resend_from": "sender@example.com"
        }))
        .unwrap();
        let numbers: Vec<_> = applied_steps(&document, "test")
            .map(|step| step.number)
            .collect();
        assert_eq!(numbers, ["4.3", "4.4", "4.4.1", "4.5", "4.6", "4.8"]);
    }

    #[test]
    fn operator_skip_requires_the_named_release_version_and_equal_values() {
        let releases = json!([{"name": "wamn", "chart": "runtime-operator-2.10.3"}]);
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
            &json!([{"name": "other", "chart": "runtime-operator-2.10.3"}]),
            Some(&desired),
            &desired
        ));
    }

    #[test]
    fn failed_wait_appends_each_owner_input_from_the_install_table() {
        let document: ClusterDocument = serde_json::from_value(json!({
            "provider": "gcp", "project_id": "test-project", "zone": "us-central1-a",
            "cluster_name": "test", "org": "test", "owner_email": "owner@example.com",
            "registry": "localhost:5000", "buckets": {"web": "test-web", "otel": "test-otel"},
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
                "registry": "localhost:5000", "buckets": {"web": "", "otel": ""},
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
