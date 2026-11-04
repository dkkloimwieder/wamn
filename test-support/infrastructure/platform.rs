//! Shared platform installation for temporary application clusters.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use tokio::process::Command;

use crate::nats_tls::ClientCertificate;

const SYSTEM_NAMESPACE: &str = "wamn-system";

/// The one NATS of a cluster that runs the platform in `wamn-system`.
pub const NATS_URL: &str = "nats://wamn-nats.wamn-system.svc.cluster.local:4222";

/// Install the shared prerequisites before the application host release.
///
/// `nats` is the one NATS with its certificates and users, which
/// `wamn_environment_control::platform::manifests::one_nats` and
/// `wamn_environment_control::platform::single_namespace_nats` render for `wamn-system`
/// (docs/plan/alignment.md R1-R5). The caller creates the cluster and its
/// private work directory first. Observed objects stay in that directory for
/// the caller to retain.
pub async fn install(
    repository: &Path,
    cluster: &str,
    work: &Path,
    namespace: &str,
    prerequisites: &[serde_json::Value],
    nats: &[serde_json::Value],
    operator: &serde_json::Value,
) -> anyhow::Result<()> {
    let kubeconfig = work.join("kubeconfig");
    let context = format!("kind-{cluster}");
    apply_objects(&kubeconfig, &context, work, "prerequisites", prerequisites).await?;
    checked(kubectl(&kubeconfig, &context).args([
        "-n",
        "cert-manager",
        "wait",
        "--for=condition=Available",
        "deployment",
        "--all",
        "--timeout=240s",
    ]))
    .await?;
    install_nats(&kubeconfig, &context, work, nats).await?;
    install_chart(&kubeconfig, &context, work, operator).await?;

    let deployment = checked(kubectl(&kubeconfig, &context).args([
        "-n",
        SYSTEM_NAMESPACE,
        "get",
        "deployment",
        "runtime-operator",
        "-o",
        "json",
    ]))
    .await?;
    std::fs::write(work.join("operator-deployment.json"), &deployment)
        .context("write observed operator deployment")?;
    validate_operator_deployment(
        &serde_json::from_slice(&deployment).context("parse operator deployment")?,
    )?;

    let role = checked(kubectl(&kubeconfig, &context).args([
        "-n",
        namespace,
        "get",
        "role",
        "wamn-runtime-operator-events",
        "-o",
        "json",
    ]))
    .await?;
    std::fs::write(work.join("operator-events-role.json"), &role)
        .context("write observed operator event Role")?;
    validate_event_role(
        &serde_json::from_slice(&role).context("parse operator event Role")?,
        namespace,
    )?;
    let binding = checked(kubectl(&kubeconfig, &context).args([
        "-n",
        namespace,
        "get",
        "rolebinding",
        "wamn-runtime-operator-events",
        "-o",
        "json",
    ]))
    .await?;
    std::fs::write(work.join("operator-events-rolebinding.json"), &binding)
        .context("write observed operator event RoleBinding")?;
    validate_event_binding(
        &serde_json::from_slice(&binding).context("parse operator event RoleBinding")?,
        namespace,
    )?;
    let collector = crate::telemetry::render_collector(repository, work)?;
    checked(
        kubectl(&kubeconfig, &context)
            .args(["apply", "-f"])
            .arg(&collector),
    )
    .await?;
    checked(kubectl(&kubeconfig, &context).args([
        "-n",
        SYSTEM_NAMESPACE,
        "wait",
        "--for=condition=Available",
        "deployment/otel-collector",
        "--timeout=120s",
    ]))
    .await?;

    // The operator namespace has the chart's certificates already. Hosts in
    // another namespace get theirs from the cluster CA. The data connection is
    // the environment's certificate (docs/plan/alignment.md R5).
    if namespace == SYSTEM_NAMESPACE {
        return Ok(());
    }
    let certificate = serde_json::json!({
        "apiVersion": "cert-manager.io/v1", "kind": "Certificate",
        "metadata": {"name": "wasmcloud-runtime-tls", "namespace": namespace},
        "spec": {
            "secretName": "wasmcloud-runtime-tls", "commonName": "wasmcloud-runtime",
            "dnsNames": ["wasmcloud-runtime"],
            "usages": ["client auth", "digital signature", "key encipherment"],
            "issuerRef": {"kind": "ClusterIssuer", "name": "wasmcloud-ca", "group": "cert-manager.io"}
        }
    });
    let certificate_path = work.join("host-runtime-certificate.json");
    write_private(&certificate_path, &serde_json::to_vec(&certificate)?)?;
    checked(
        kubectl(&kubeconfig, &context)
            .args(["apply", "-f"])
            .arg(&certificate_path),
    )
    .await?;
    checked(kubectl(&kubeconfig, &context).args([
        "-n",
        namespace,
        "wait",
        "--for=condition=Ready",
        "certificate/wasmcloud-runtime-tls",
        "--timeout=120s",
    ]))
    .await?;
    Ok(())
}

/// Apply objects supplied by the caller's production render adapter.
pub(crate) async fn apply_objects(
    kubeconfig: &Path,
    context: &str,
    work: &Path,
    name: &str,
    objects: &[serde_json::Value],
) -> anyhow::Result<()> {
    let path = work.join(format!("{name}.json"));
    write_private(
        &path,
        &serde_json::to_vec(&serde_json::json!({"apiVersion":"v1","kind":"List","items":objects}))?,
    )?;
    checked(
        kubectl(kubeconfig, context)
            .args(["apply", "--server-side", "-f"])
            .arg(path),
    )
    .await?;
    Ok(())
}

/// Install the caller's chart entry without constructing chart values here.
pub(crate) async fn install_chart(
    kubeconfig: &Path,
    context: &str,
    work: &Path,
    entry: &serde_json::Value,
) -> anyhow::Result<()> {
    let text = |key: &str| {
        entry[key]
            .as_str()
            .with_context(|| format!("chart entry {key}"))
    };
    let values = work.join(format!("values-{}.yaml", text("name")?));
    write_private(&values, serde_yaml::to_string(&entry["values"])?.as_bytes())?;
    checked(
        Command::new("helm")
            .args([
                "upgrade",
                "--install",
                text("name")?,
                text("chart")?,
                "--version",
                text("version")?,
                "--namespace",
                text("namespace")?,
                "--create-namespace",
                "--kubeconfig",
            ])
            .arg(kubeconfig)
            .args(["--kube-context", context, "-f"])
            .arg(values)
            .args(["--wait", "--timeout", "5m"]),
    )
    .await?;
    Ok(())
}

/// Apply the CA and the certificates, wait for them, then the users and the
/// server, and wait for the server.
pub(crate) async fn install_nats(
    kubeconfig: &Path,
    context: &str,
    work: &Path,
    nats: &[serde_json::Value],
) -> anyhow::Result<()> {
    let namespace = serde_json::json!({
        "apiVersion": "v1", "kind": "Namespace", "metadata": {"name": SYSTEM_NAMESPACE}
    });
    let (mut certificates, server): (Vec<_>, Vec<_>) = nats.iter().partition(|object| {
        matches!(
            object["kind"].as_str(),
            Some("ClusterIssuer" | "Certificate")
        )
    });
    certificates.insert(0, &namespace);
    for (name, objects) in [("certificates", certificates), ("server", server)] {
        let file = work.join(format!("one-nats-{name}.json"));
        write_private(
            &file,
            &serde_json::to_vec(&serde_json::json!({
                "apiVersion": "v1", "kind": "List", "items": objects
            }))?,
        )?;
        checked(
            kubectl(kubeconfig, context)
                .args(["apply", "-f"])
                .arg(&file),
        )
        .await?;
        if name == "certificates" {
            checked(kubectl(kubeconfig, context).args([
                "wait",
                "--for=condition=Ready",
                "clusterissuer/wasmcloud-ca",
                "--timeout=120s",
            ]))
            .await?;
            checked(kubectl(kubeconfig, context).args([
                "-n",
                SYSTEM_NAMESPACE,
                "wait",
                "--for=condition=Ready",
                "certificate",
                "--all",
                "--timeout=180s",
            ]))
            .await?;
        }
    }
    checked(kubectl(kubeconfig, context).args([
        "-n",
        SYSTEM_NAMESPACE,
        "rollout",
        "status",
        "statefulset/wamn-nats",
        "--timeout=300s",
    ]))
    .await?;
    Ok(())
}

/// The one NATS of a cluster that [`install`] set up, through a port forward
/// to this machine, with the certificates of the platform clients that a
/// case acts as. The server certificate names 127.0.0.1 for the forward.
#[derive(Debug)]
pub struct ClusterNats {
    /// The forwarded URL on this machine.
    pub server: String,
    /// The provisioning Jobs: stream and consumer management.
    pub provisioning: ClientCertificate,
    /// The CDC reader: the environment's events.
    pub publisher: ClientCertificate,
    /// `wamn-ctl`: stream and consumer information.
    pub observer: ClientCertificate,
    /// The environment user of the hosts and the materializer.
    pub environment: ClientCertificate,
    _forward: tokio::process::Child,
}

/// Forward the one NATS of `cluster` and read the client certificates that
/// cert-manager issued, as private files in `work`. `env_key` names the
/// environment certificate, which lives in the hosts namespace `hosts`
/// (docs/plan/alignment.md R4, R5).
pub async fn cluster_nats(
    work: &Path,
    cluster: &str,
    env_key: &str,
    hosts: &str,
) -> anyhow::Result<ClusterNats> {
    let kubeconfig = work.join("kubeconfig");
    let context = format!("kind-{cluster}");
    let environment = format!("wamn-nats-{env_key}");
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let forward = crate::cluster::spawn(kubectl(&kubeconfig, &context).args([
        "-n",
        SYSTEM_NAMESPACE,
        "port-forward",
        "svc/wamn-nats",
        &format!("{port}:4222"),
    ]))
    .context("forward the one NATS")?;
    let nats = ClusterNats {
        server: format!("nats://127.0.0.1:{port}"),
        provisioning: certificate(
            &kubeconfig,
            &context,
            work,
            SYSTEM_NAMESPACE,
            "wamn-provisioning-nats",
            "wamn-provisioning",
            "_INBOX",
        )
        .await?,
        publisher: certificate(
            &kubeconfig,
            &context,
            work,
            SYSTEM_NAMESPACE,
            "wamn-cdc-reader-nats",
            "wamn-cdc-reader",
            "_INBOX",
        )
        .await?,
        observer: certificate(
            &kubeconfig,
            &context,
            work,
            SYSTEM_NAMESPACE,
            "wamn-ctl-nats",
            "wamn-ctl",
            "_INBOX",
        )
        .await?,
        environment: certificate(
            &kubeconfig,
            &context,
            work,
            hosts,
            &environment,
            &wamn_control_registry::environment_nats_user(env_key),
            &format!(
                "_INBOX_{}",
                wamn_control_registry::environment_nats_user(env_key)
            ),
        )
        .await?,
        _forward: forward,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match nats.provisioning.connect(&nats.server).await {
            Ok(client) => {
                client
                    .drain()
                    .await
                    .context("close the forward check client")?;
                return Ok(nats);
            }
            Err(_) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// The certificate of the Secret `name` in `namespace`, as private files in
/// `work`, for `user` with its reply prefix `inbox`.
async fn certificate(
    kubeconfig: &Path,
    context: &str,
    work: &Path,
    namespace: &str,
    name: &str,
    user: &str,
    inbox: &str,
) -> anyhow::Result<ClientCertificate> {
    use base64::Engine as _;
    let secret: serde_json::Value = serde_json::from_slice(
        &checked(
            kubectl(kubeconfig, context)
                .args(["-n", namespace, "get", "secret", name, "-o", "json"]),
        )
        .await?,
    )
    .with_context(|| format!("parse Secret {name}"))?;
    let mut files = Vec::new();
    for key in ["ca.crt", "tls.crt", "tls.key"] {
        let file = work.join(format!("{name}-{key}"));
        let encoded = secret["data"][key]
            .as_str()
            .with_context(|| format!("Secret {name} has no {key}"))?;
        write_private(
            &file,
            &base64::engine::general_purpose::STANDARD.decode(encoded)?,
        )?;
        files.push(file);
    }
    let [ca, cert, key] = <[std::path::PathBuf; 3]>::try_from(files)
        .map_err(|_| anyhow::anyhow!("three certificate files"))?;
    Ok(ClientCertificate {
        user: user.to_owned(),
        inbox: inbox.to_owned(),
        ca,
        cert,
        key,
    })
}

fn kubectl(kubeconfig: &Path, context: &str) -> Command {
    let mut command = Command::new("kubectl");
    command
        .arg("--kubeconfig")
        .arg(kubeconfig)
        .arg("--context")
        .arg(context);
    command
}

async fn checked(command: &mut Command) -> anyhow::Result<Vec<u8>> {
    crate::cluster::run(command).await
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create private CA file {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write private CA file {}", path.display()))
}

fn validate_operator_deployment(deployment: &OperatorDeployment) -> anyhow::Result<()> {
    ensure!(
        deployment.metadata.name == "runtime-operator"
            && deployment.metadata.namespace == SYSTEM_NAMESPACE
            && deployment.spec.template.spec.service_account_name == "wamn-runtime-operator",
        "operator deployment has the wrong name, namespace, or ServiceAccount"
    );
    Ok(())
}

fn validate_event_role(role: &EventRole, namespace: &str) -> anyhow::Result<()> {
    ensure!(
        role.metadata.name == "wamn-runtime-operator-events"
            && role.metadata.namespace == namespace,
        "operator event Role has the wrong name or namespace"
    );
    ensure!(
        role.rules.len() == 1,
        "operator event Role must contain exactly one rule"
    );
    let first_rule = &role.rules[0];
    let mut verbs = first_rule.verbs.clone();
    verbs.sort();
    ensure!(
        first_rule.api_groups == ["events.k8s.io"]
            && first_rule.resources == ["events"]
            && verbs == ["create", "patch"],
        "operator event Role must grant only create and patch on events.k8s.io events"
    );
    Ok(())
}

fn validate_event_binding(binding: &EventBinding, namespace: &str) -> anyhow::Result<()> {
    ensure!(
        binding.metadata.name == "wamn-runtime-operator-events"
            && binding.metadata.namespace == namespace,
        "operator event RoleBinding has the wrong name or namespace"
    );
    ensure!(
        binding.role_ref
            == RoleRef {
                api_group: "rbac.authorization.k8s.io".into(),
                kind: "Role".into(),
                name: "wamn-runtime-operator-events".into()
            },
        "operator event RoleBinding must name the environment event Role"
    );
    ensure!(
        binding.subjects
            == [Subject {
                kind: "ServiceAccount".into(),
                name: "wamn-runtime-operator".into(),
                namespace: SYSTEM_NAMESPACE.into()
            }],
        "operator event RoleBinding must name only the existing operator ServiceAccount"
    );
    Ok(())
}

type Extra = BTreeMap<String, Value>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Metadata {
    name: String,
    namespace: String,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
#[cfg(test)]
enum EventRbac {
    Role(EventRole),
    RoleBinding(EventBinding),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventRole {
    api_version: String,
    metadata: Metadata,
    rules: Vec<Rule>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Rule {
    api_groups: Vec<String>,
    resources: Vec<String>,
    verbs: Vec<String>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventBinding {
    api_version: String,
    metadata: Metadata,
    role_ref: RoleRef,
    subjects: Vec<Subject>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RoleRef {
    api_group: String,
    kind: String,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Subject {
    kind: String,
    name: String,
    namespace: String,
}

#[derive(Debug, Deserialize)]
struct OperatorDeployment {
    metadata: Metadata,
    spec: OperatorSpec,
}

#[derive(Debug, Deserialize)]
struct OperatorSpec {
    template: OperatorTemplate,
}

#[derive(Debug, Deserialize)]
struct OperatorTemplate {
    spec: OperatorPod,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OperatorPod {
    service_account_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const RBAC: &str =
        include_str!("../../deploy/platform/runtime-operator-events-rbac.example.yaml");

    fn rbac() -> Vec<EventRbac> {
        wamn_environment_control::platform::install::runtime_events(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
            "test-warehouse",
            SYSTEM_NAMESPACE,
        )
        .unwrap()
        .into_iter()
        .map(|object| serde_json::from_value(object).unwrap())
        .collect()
    }

    #[test]
    fn event_permissions_bind_only_the_existing_operator_in_the_selected_namespace() {
        let documents = rbac();
        assert_eq!(documents.len(), 2);
        let EventRbac::Role(role) = &documents[0] else {
            panic!("Role")
        };
        let EventRbac::RoleBinding(binding) = &documents[1] else {
            panic!("RoleBinding")
        };
        validate_event_role(role, "test-warehouse").unwrap();
        validate_event_binding(binding, "test-warehouse").unwrap();
        assert!(validate_event_role(role, "other-warehouse").is_err());
        assert!(validate_event_binding(binding, "other-warehouse").is_err());
        let before = serde_yaml::Deserializer::from_str(RBAC)
            .map(|document| Value::deserialize(document).unwrap())
            .collect::<Vec<_>>();
        let after = rbac()
            .iter()
            .map(|object| serde_yaml::to_value(object).unwrap())
            .collect::<Vec<_>>();
        for (mut expected, actual) in before.into_iter().zip(after) {
            expected["metadata"]["namespace"] = Value::String("test-warehouse".into());
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn observed_operator_must_keep_its_name_namespace_and_service_account() {
        let original = json!({"metadata":{"name":"runtime-operator","namespace":"wamn-system"},
            "spec":{"template":{"spec":{"serviceAccountName":"wamn-runtime-operator"}}}});
        validate_operator_deployment(&serde_json::from_value(original.clone()).unwrap()).unwrap();
        for field in ["name", "namespace", "serviceAccountName"] {
            let mut changed = original.clone();
            if field == "serviceAccountName" {
                changed["spec"]["template"]["spec"][field] = json!("other");
            } else {
                changed["metadata"][field] = json!("other");
            }
            assert!(
                validate_operator_deployment(&serde_json::from_value(changed).unwrap()).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn observed_role_refuses_changed_groups_resources_verbs_and_rule_count() {
        let EventRbac::Role(original) = &rbac()[0] else {
            panic!("Role")
        };
        for field in ["groups", "resources", "verbs", "rules"] {
            let mut role = original.clone();
            match field {
                "groups" => role.rules[0].api_groups = vec![String::new()],
                "resources" => role.rules[0].resources = vec!["secrets".into()],
                "verbs" => role.rules[0].verbs.push("get".into()),
                "rules" => role.rules.push(role.rules[0].clone()),
                _ => unreachable!(),
            }
            assert!(
                validate_event_role(&role, "test-warehouse").is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn observed_binding_refuses_other_roles_or_service_accounts() {
        let EventRbac::RoleBinding(original) = &rbac()[1] else {
            panic!("RoleBinding")
        };
        let mut wrong_role = original.clone();
        wrong_role.role_ref.name = "other".into();
        assert!(validate_event_binding(&wrong_role, "test-warehouse").is_err());
        let mut wrong_subject = original.clone();
        wrong_subject.subjects[0].namespace = "other".into();
        assert!(validate_event_binding(&wrong_subject, "test-warehouse").is_err());
        let mut extra_subject = original.clone();
        extra_subject.subjects.push(original.subjects[0].clone());
        assert!(validate_event_binding(&extra_subject, "test-warehouse").is_err());
    }
}
