//! Environment apply runs in the cluster with its declared inputs.

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
    registry: Option<&wamn_package_artifact::PackageRegistry>,
) -> anyhow::Result<bool> {
    let initial = registry.is_none();
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
        if initial {
            environment.release = DeclaredRelease::None;
        }
        let tenant = wamn_control_registry::project_env_tenant(
            &environment.org,
            &environment.project,
            &environment.env,
        );
        let mut request = JobRequest {
            name: format!(
                "platform-{}apply-{tenant}",
                if initial { "first-" } else { "" }
            ),
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
        if let Some(registry) = registry {
            release_inputs(
                target,
                document,
                &environment,
                environments
                    .parent()
                    .context("environments has no cluster directory")?,
                directory,
                &mut request,
                registry,
            )
            .await?;
        }
        let result = job::run(
            target,
            directory,
            &request,
            &json!({"environment": environment}),
            std::slice::from_ref(&admin),
        )
        .await;
        match result {
            Ok(applied) => changed |= applied,
            Err(error) => {
                if let DeclaredRelease::Digest(digest) = &environment.release
                    && matches!(
                        error.downcast_ref::<job::JobError>(),
                        Some(job::JobError { kind: job::JobErrorKind::JobFailed, name })
                            if name == &request.name
                    )
                {
                    let log = std::fs::read_to_string(
                        directory.join("jobs").join(format!("{}.log", request.name)),
                    )
                    .unwrap_or_default();
                    if let Some(message) = qualification_refusal(&log, digest) {
                        let registry = registry.context("release apply has no registry inputs")?;
                        anyhow::bail!(
                            "{message}\nRun on the build machine: {}",
                            qualification_command(digest, directory, registry)?
                        );
                    }
                }
                return Err(error);
            }
        }
    }
    Ok(changed)
}

fn qualification_refusal<'a>(log: &'a str, digest: &str) -> Option<&'a str> {
    let prefix = format!("release {digest} has no qualification on the chart's image set:");
    log.lines()
        .find_map(|line| line.find(&prefix).map(|at| &line[at..]))
}

fn qualification_command(
    digest: &str,
    directory: &Path,
    registry: &wamn_package_artifact::PackageRegistry,
) -> anyhow::Result<String> {
    let mut command = format!(
        "wamn-ctl qualify-release --release-digest {digest} --release-chart {} --artifact-base {} --registry-auth-file {}",
        shell_path(&directory.join("release-chart")),
        shell_path(Path::new(&super::packages::artifact_base(
            registry, "releases"
        )?)),
        shell_path(&registry.registry_auth_file)
    );
    for path in &registry.oci_ca_paths {
        command.push_str(" --oci-ca-path ");
        command.push_str(&shell_path(path));
    }
    Ok(command)
}

fn shell_path(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\"'\"'"))
}

pub(super) fn chart_files(
    root: &Path,
    relative: &Path,
    request: &mut JobRequest,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let path = relative.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            chart_files(root, &path, request)?;
        } else {
            let name = path.to_str().context("chart path is not UTF-8")?.to_owned();
            if !request.files.contains_key(&name) {
                request
                    .binary_files
                    .insert(name, std::fs::read(entry.path())?);
            }
        }
    }
    Ok(())
}

async fn release_inputs(
    target: &Target,
    document: &ClusterDocument,
    environment: &crate::environment::document::EnvironmentDocument,
    cluster_directory: &Path,
    directory: &Path,
    request: &mut JobRequest,
    registry: &wamn_package_artifact::PackageRegistry,
) -> anyhow::Result<()> {
    super::packages::registry_inputs(request, registry)?;
    request.environment.insert(
        "WAMN_PACKAGE_ARTIFACT_BASE".into(),
        registry.artifact_base.clone(),
    );
    request.environment.insert(
        "WAMN_RELEASE_ARTIFACT_BASE".into(),
        super::packages::artifact_base(registry, "releases")?,
    );
    chart_files(&directory.join("release-chart"), Path::new(""), request)?;
    let tenant = wamn_control_registry::project_env_tenant(
        &environment.org,
        &environment.project,
        &environment.env,
    );
    let secret: SecretData = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "secret",
        &format!("evt-nats-{tenant}-provisioning"),
    )
    .await?
    .context("missing event provisioning Secret")?;
    request.environment.insert(
        "WAMN_EVENT_NATS_URL".into(),
        "nats://evt-nats.platform.svc.cluster.local:4222".into(),
    );
    request
        .files
        .insert("event-password".into(), secret_text(&secret, "password")?);
    request.environment.insert(
        "WAMN_EVENT_NATS_USERNAME".into(),
        secret_text(&secret, "username")?,
    );
    request.environment.insert(
        "WAMN_EVENT_NATS_PASSWORD_FILE".into(),
        "/inputs/event-password".into(),
    );
    let path = cluster_directory.join(format!("host-group-{}.yaml", environment.project));
    let mut group: serde_json::Value = serde_yaml::from_slice(
        &std::fs::read(&path).with_context(|| format!("read host group {}", path.display()))?,
    )?;
    let secrets = super::packages::credentials(target, environment).await?;
    let database = url::Url::parse(&secrets[1].value)
        .map_err(|_| anyhow::anyhow!("parse environment database URL"))?;
    let suffix = database
        .path()
        .rsplit_once("--")
        .context("environment database has no instance suffix")?
        .1;
    for variable in group["env"]
        .as_array_mut()
        .context("host group has no env list")?
    {
        let replacement = match variable["name"].as_str().unwrap_or_default() {
            "WAMN_COMPONENT_ARTIFACT_BASE" => Some(format!("{}/components", document.registry)),
            "WAMN_EVT_ORG" => Some(environment.org.clone()),
            "WAMN_EVT_PROJECT" => Some(environment.project.clone()),
            "WAMN_EVT_ENV" => Some(environment.env.clone()),
            "WAMN_SESSION_INSTANCE_SUFFIX" => Some(suffix.into()),
            _ => None,
        };
        if let Some(value) = replacement {
            variable["value"] = value.into();
        }
        if variable["name"] == "WAMN_EVT_NATS_USERNAME" {
            variable["valueFrom"]["secretKeyRef"]["name"] =
                format!("evt-nats-{tenant}-runtime").into();
        }
    }
    for volume in group["volumes"]
        .as_array_mut()
        .context("host group has no volumes")?
    {
        match volume["name"].as_str().unwrap_or_default() {
            "event-nats" => {
                volume["secret"]["secretName"] = format!("evt-nats-{tenant}-runtime").into();
            }
            "materializer-nats" => {
                volume["secret"]["secretName"] = format!("evt-nats-{tenant}-materializer").into();
            }
            _ => {}
        }
    }
    request
        .files
        .insert("host-group.yaml".into(), serde_yaml::to_string(&group)?);
    request.environment.insert(
        "WAMN_RELEASE_HOST_GROUP".into(),
        "/inputs/host-group.yaml".into(),
    );
    let hosts = request
        .permissions
        .iter_mut()
        .find(|access| access.namespace == "hosts")
        .context("missing hosts permissions")?;
    hosts.rules.extend([
        PolicyRule {
            api_groups: vec![String::new()],
            resources: vec!["services".into(), "configmaps".into(), "secrets".into()],
            verbs: [
                "get", "list", "watch", "create", "patch", "update", "delete",
            ]
            .map(str::to_owned)
            .into(),
        },
        PolicyRule {
            api_groups: vec!["apps".into()],
            resources: vec!["deployments".into(), "replicasets".into()],
            verbs: [
                "get", "list", "watch", "create", "patch", "update", "delete",
            ]
            .map(str::to_owned)
            .into(),
        },
        PolicyRule {
            api_groups: vec!["networking.k8s.io".into()],
            resources: vec!["networkpolicies".into()],
            verbs: [
                "get", "list", "watch", "create", "patch", "update", "delete",
            ]
            .map(str::to_owned)
            .into(),
        },
        PolicyRule {
            api_groups: vec!["runtime.wasmcloud.dev".into()],
            resources: vec!["workloaddeployments".into()],
            verbs: [
                "get", "list", "watch", "create", "patch", "update", "delete",
            ]
            .map(str::to_owned)
            .into(),
        },
    ]);
    Ok(())
}

pub(super) fn kubeconfig() -> serde_json::Value {
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
                resources: vec!["clusters".into(), "databases".into(), "publications".into()],
                verbs: ["get", "list", "watch", "create", "patch"]
                    .map(str::to_owned)
                    .into(),
            }],
        },
        Access {
            namespace: "hosts".into(),
            rules: vec![
                PolicyRule {
                    api_groups: vec![String::new()],
                    resources: vec!["secrets".into()],
                    verbs: ["get", "list", "watch", "create", "patch"]
                        .map(str::to_owned)
                        .into(),
                },
                PolicyRule {
                    api_groups: vec![String::new()],
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
    fn qualification_command_keeps_the_host_chart_registry_and_ca_paths() {
        let registry = wamn_package_artifact::PackageRegistry {
            artifact_base: "registry.example/packages".into(),
            registry_auth_file: "/host/registry.json".into(),
            insecure_registry: false,
            oci_ca_paths: vec!["/host/ca.crt".into()],
        };
        assert_eq!(
            qualification_command("sha256:abc", Path::new("/host/work"), &registry).unwrap(),
            "wamn-ctl qualify-release --release-digest sha256:abc --release-chart '/host/work/release-chart' --artifact-base 'registry.example/releases' --registry-auth-file '/host/registry.json' --oci-ca-path '/host/ca.crt'"
        );
    }

    #[test]
    fn qualification_message_is_the_jobs_refusal_for_the_requested_release() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let message = format!(
            "release {digest} has no qualification on the chart's image set: registry/releases:qualification-example is absent"
        );
        let log = format!("Error: refused\n\nCaused by:\n    {message}\n");
        assert_eq!(qualification_refusal(&log, &digest), Some(message.as_str()));
        assert_eq!(qualification_refusal(&log, "sha256:other"), None);
        assert_eq!(
            qualification_refusal("database connection refused", &digest),
            None
        );
    }

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
