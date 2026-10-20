//! The first environment apply runs in the cluster with its declared inputs.

use std::path::Path;

use anyhow::Context as _;
use serde_json::json;

use crate::environment::document::DeclaredRelease;
use crate::{kubectl, release_chart::Target};

use super::document::ClusterDocument;
use super::install::{DATABASE_HOST, SecretData, administrator, runner_image, secret_text};
use super::job::{self, Access, JobRequest, PolicyRule};

pub(super) async fn apply(
    target: &Target,
    document: &ClusterDocument,
    environments: &Path,
    directory: &Path,
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
    let mut admin = administrator(&secret)?;
    admin.environment = "WAMN_CONTROL_DATABASE_URL".into();
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
    for (_, mut environment) in super::broker::documents(environments)? {
        environment.release = DeclaredRelease::None;
        let tenant = format!(
            "{}--{}--{}",
            environment.org, environment.project, environment.env
        );
        let mut request = JobRequest {
            name: format!("platform-first-apply-{tenant}"),
            image: image.clone(),
            arguments: vec!["env".into(), "apply".into(), "/inputs/environment.k".into()],
            calls_kubernetes: true,
            environment: [
                ("KUBECONFIG".into(), "/inputs/kubeconfig".into()),
                ("WAMN_KCL".into(), "/usr/local/bin/kcl".into()),
                ("WAMN_RELEASE_NAMESPACE".into(), "hosts".into()),
                ("WAMN_DATABASE_NAMESPACE".into(), "platform".into()),
                ("WAMN_DATABASE_HOST".into(), DATABASE_HOST.into()),
                ("WAMN_RELEASE_CHART".into(), "/inputs".into()),
                (
                    "WAMN_RELEASE_ARTIFACT_BASE".into(),
                    format!("{}/releases", document.registry),
                ),
                (
                    "WAMN_PACKAGE_ARTIFACT_BASE".into(),
                    format!("{}/packages", document.registry),
                ),
                (
                    "WAMN_REGISTRY_AUTH_FILE".into(),
                    "/inputs/registry.json".into(),
                ),
                (
                    "WAMN_AUTH_NATS_URL".into(),
                    "nats://nats.platform.svc.cluster.local:4222".into(),
                ),
                (
                    "WAMN_AUTH_NATS_TLS_SERVER_NAME".into(),
                    "nats.platform.svc.cluster.local".into(),
                ),
                ("WAMN_AUTH_NATS_TLS_CA".into(), "/inputs/ca.crt".into()),
                ("WAMN_AUTH_NATS_TLS_CERT".into(), "/inputs/tls.crt".into()),
                ("WAMN_AUTH_NATS_TLS_KEY".into(), "/inputs/tls.key".into()),
            ]
            .into(),
            files: [
                ("environment.k".into(), environment.to_kcl()),
                ("kubeconfig".into(), kubeconfig().to_string()),
                ("registry.json".into(), "{}".into()),
            ]
            .into(),
            permissions: permissions(),
            ..JobRequest::default()
        };
        for key in ["ca.crt", "tls.crt", "tls.key"] {
            request.files.insert(key.into(), secret_text(&auth, key)?);
        }
        for name in ["Chart.yaml", "values.yaml"] {
            request.files.insert(
                name.into(),
                std::fs::read_to_string(directory.join("release-chart").join(name))?,
            );
        }
        changed |= job::run(
            target,
            directory,
            &request,
            &json!({"environment": environment}),
            std::slice::from_ref(&admin),
        )
        .await?;
    }
    Ok(changed)
}

fn kubeconfig() -> serde_json::Value {
    json!({
        "apiVersion": "v1", "kind": "Config",
        "clusters": [{"name": "install", "cluster": {
            "server": "https://kubernetes.default.svc",
            "certificate-authority": "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt"
        }}],
        "users": [{"name": "install", "user": {
            "tokenFile": "/var/run/secrets/kubernetes.io/serviceaccount/token"
        }}],
        "contexts": [{"name": "install", "context": {"cluster": "install", "user": "install"}}],
        "current-context": "install"
    })
}

fn permissions() -> Vec<Access> {
    vec![
        Access {
            namespace: "platform".into(),
            rules: vec![PolicyRule {
                api_groups: vec!["postgresql.cnpg.io".into()],
                resources: vec!["clusters".into(), "databases".into()],
                verbs: ["get", "list", "watch", "create", "patch"]
                    .map(str::to_owned)
                    .into(),
            }],
        },
        Access {
            namespace: "hosts".into(),
            rules: vec![
                PolicyRule {
                    api_groups: vec!["".into()],
                    resources: vec!["secrets".into()],
                    verbs: ["get", "list", "watch", "create", "patch"]
                        .map(str::to_owned)
                        .into(),
                },
                PolicyRule {
                    api_groups: vec!["".into()],
                    resources: vec!["pods".into()],
                    verbs: ["get", "list", "watch"].map(str::to_owned).into(),
                },
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_configuration_uses_its_service_account_and_namespace_access() {
        let config = kubeconfig();
        assert_eq!(
            config["users"][0]["user"]["tokenFile"],
            "/var/run/secrets/kubernetes.io/serviceaccount/token"
        );
        assert_eq!(
            config["clusters"][0]["cluster"]["server"],
            "https://kubernetes.default.svc"
        );
        let access = permissions();
        assert_eq!(access[0].namespace, "platform");
        assert_eq!(access[1].namespace, "hosts");
        assert!(access.iter().flat_map(|access| &access.rules).all(|rule| {
            !rule
                .verbs
                .iter()
                .any(|verb| verb == "*" || verb == "delete")
        }));
    }
}
