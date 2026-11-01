//! Shared platform installation for temporary application clusters.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use tokio::process::Command;

const SYSTEM_NAMESPACE: &str = "wamn-system";

/// The one NATS of a cluster that runs the platform in `wamn-system`.
pub const NATS_URL: &str = "nats://wamn-nats.wamn-system.svc.cluster.local:4222";

/// Install the shared prerequisites before the application host release.
///
/// `nats` is the one NATS with its certificates and users, which
/// `wamn_control::platform::manifests::one_nats` and
/// `wamn_control::platform::single_namespace_nats` render for `wamn-system`
/// (docs/plan/alignment.md R1-R5). The caller creates the cluster and its
/// private work directory first. Observed objects stay in that directory for
/// the caller to retain.
pub async fn install(
    repository: &Path,
    lifecycle: &Path,
    cluster: &str,
    work: &Path,
    namespace: &str,
    operator_values: &Path,
    nats: &[serde_json::Value],
) -> anyhow::Result<()> {
    let kubeconfig = work.join("kubeconfig");
    let context = format!("kind-{cluster}");
    checked(
        kubectl(&kubeconfig, &context)
            .args(["apply", "-f"])
            .arg(repository.join("deploy/infra/cert-manager.yaml")),
    )
    .await?;
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
    // The operator and every host group reach the one NATS.
    let mut base: Value = serde_yaml::from_str(
        &std::fs::read_to_string(repository.join("deploy/infra/values-wamn.yaml"))
            .context("read operator base values")?,
    )?;
    base["global"]["nats"] = serde_yaml::from_str(&format!(
        "{{schedulerUrl: \"{NATS_URL}\", dataUrl: \"{NATS_URL}\"}}"
    ))?;
    std::fs::write(
        work.join("operator-base.yaml"),
        serde_yaml::to_string(&base)?,
    )
    .context("write operator base values")?;
    checked(
        Command::new(lifecycle)
            .arg("install-operator")
            .arg(cluster)
            .arg(work)
            .arg(operator_values),
    )
    .await?;

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

    let rbac = render_event_rbac(
        &std::fs::read_to_string(
            repository.join("deploy/platform/runtime-operator-events-rbac.example.yaml"),
        )
        .context("read operator event permissions template")?,
        namespace,
    )?;
    let rbac_path = work.join("runtime-operator-events-rbac.yaml");
    std::fs::write(&rbac_path, rbac).context("write operator event permissions")?;
    checked(
        kubectl(&kubeconfig, &context)
            .args(["apply", "-f"])
            .arg(&rbac_path),
    )
    .await?;
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
    // The platform-owned ServiceAccount every release chart host runs as.
    checked(
        kubectl(&kubeconfig, &context)
            .args(["-n", namespace, "apply", "-f"])
            .arg(repository.join("deploy/platform/wamn-host-serviceaccount.yaml")),
    )
    .await?;

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

/// Apply the CA and the certificates, wait for them, then the users and the
/// server, and wait for the server.
async fn install_nats(
    kubeconfig: &Path,
    context: &str,
    work: &Path,
    nats: &[serde_json::Value],
) -> anyhow::Result<()> {
    checked(kubectl(kubeconfig, context).args(["create", "namespace", SYSTEM_NAMESPACE])).await?;
    let (certificates, server): (Vec<_>, Vec<_>) = nats.iter().partition(|object| {
        matches!(
            object["kind"].as_str(),
            Some("ClusterIssuer" | "Certificate")
        )
    });
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
    let program = command.as_std().get_program().to_owned();
    let output = command
        .output()
        .await
        .with_context(|| format!("run {}", program.to_string_lossy()))?;
    ensure!(
        output.status.success(),
        "{} failed with {}: {}",
        program.to_string_lossy(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
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

fn render_event_rbac(template: &str, namespace: &str) -> anyhow::Result<String> {
    let mut documents = serde_yaml::Deserializer::from_str(template)
        .map(EventRbac::deserialize)
        .collect::<Result<Vec<_>, _>>()
        .context("parse operator event permissions")?;
    for document in &mut documents {
        match document {
            EventRbac::Role(role) => namespace.clone_into(&mut role.metadata.namespace),
            EventRbac::RoleBinding(binding) => {
                namespace.clone_into(&mut binding.metadata.namespace);
            }
        }
    }
    Ok(documents
        .iter()
        .map(serde_yaml::to_string)
        .collect::<Result<Vec<_>, _>>()?
        .join("---\n"))
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
        serde_yaml::Deserializer::from_str(&render_event_rbac(RBAC, "test-warehouse").unwrap())
            .map(|document| EventRbac::deserialize(document).unwrap())
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
        let output = render_event_rbac(RBAC, "test-warehouse").unwrap();
        let after = serde_yaml::Deserializer::from_str(&output)
            .map(|document| Value::deserialize(document).unwrap())
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
