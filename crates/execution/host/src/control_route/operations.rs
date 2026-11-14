//! Server-authorized application Jobs and bounded Kubernetes observations.

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};
use wamn_catalog::HostAttachment;
use wamn_identity_protocol::PrincipalId;
use wamn_kubernetes::kubernetes_api::KubernetesApi;

use super::{ControlRoutes, Refusal};

const TARGET: &str = "wamn.dev/operation-target";
const OPERATION: &str = "wamn.dev/operation";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    project: String,
    env: String,
    #[serde(default)]
    application: Option<String>,
    #[serde(default)]
    release: Option<String>,
    #[serde(default)]
    job: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum Operation {
    Deploy,
    Rollback,
    Stop,
    Retain,
    Diff,
    Apply,
    Reset,
}

impl Operation {
    fn parse(operation: &str) -> Option<Self> {
        Some(match operation {
            "application.deploy" => Self::Deploy,
            "application.rollback" => Self::Rollback,
            "application.stop" => Self::Stop,
            "release.retain" => Self::Retain,
            "schema.diff" => Self::Diff,
            "schema.apply" => Self::Apply,
            "schema.reset" => Self::Reset,
            _ => return None,
        })
    }

    fn command(self) -> &'static str {
        match self {
            Self::Deploy => "deploy",
            Self::Rollback => "rollback",
            Self::Stop => "stop",
            Self::Retain => "retain",
            Self::Diff => "diff",
            Self::Apply => "apply",
            Self::Reset => "reset",
        }
    }

    fn privileged(self) -> bool {
        matches!(self, Self::Apply | Self::Reset)
    }

    fn needs(
        self,
        org: &str,
        project: &str,
        env: &str,
        database: &str,
        apply_credential: &str,
    ) -> Vec<(String, &'static str, &'static str)> {
        let mut needs = vec![
            (apply_credential.into(), "apply", "/credentials/apply"),
            ("wamn-ctl-nats".into(), "nats", "/credentials/nats"),
        ];
        if self.privileged() {
            let owner = wamn_control_provision::migrations::MigrationDatabase::Project
                .managed_role(database);
            needs.push((owner.password_secret.name, "owner", "/credentials/owner"));
            needs.push((
                wamn_control_provision::project_env_cdc_secret_name(org, project, env),
                "capture",
                "/credentials/capture",
            ));
            needs.push((
                wamn_control_provision::workload_secret_name(
                    wamn_control_provision::WorkloadRoleFamily::Migration,
                    org,
                    project,
                    env,
                ),
                "migration",
                "/credentials/migration",
            ));
        }
        needs
    }
}

fn invalid(field: &'static str) -> Refusal {
    Refusal::Declared {
        code: "invalid_input",
        detail: json!({"field":field}),
    }
}

impl ControlRoutes<'_> {
    pub(super) async fn operation_job(
        &self,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        payload: &str,
    ) -> Result<Value, Refusal> {
        let request: Request = serde_json::from_str(payload).map_err(|_| invalid("value"))?;
        let coordinate = wamn_control_provision::environment_names::environment_resource_name(
            self.org,
            &request.project,
            &request.env,
        )
        .map_err(|_| invalid("env"))?;
        // Repeat authority in the write transaction before contacting Kubernetes.
        let mut writer = self.writer.lock().await;
        let mut transaction = self
            .begin_in_environment(
                &mut writer,
                attachment,
                caller,
                Some(&request.project),
                Some(&request.env),
            )
            .await?;
        let exists = sqlx::query(
            wamn_registry::sql::EXECUTION_HOST_SRC_CONTROL_ROUTE_OPERATION_JOB_OPERATION_JOB,
        )
        .bind(self.org)
        .bind(&request.project)
        .bind(&request.env)
        .fetch_optional(&mut *transaction)
        .await?
        .is_some();
        if !exists {
            return Err(Refusal::Declared {
                code: "environment_not_found",
                detail: json!({"field":"env"}),
            });
        }
        transaction.commit().await?;
        drop(writer);

        let api;
        let api = if let Some(api) = self.operation_jobs {
            api
        } else {
            api = KubernetesApi::in_cluster().map_err(anyhow::Error::from)?;
            &api
        };
        let namespace = &api.namespace;
        if namespace != self.org {
            return Err(anyhow::anyhow!("operation worker must share the org namespace").into());
        }
        if attachment.route.operation == "operation.status" {
            if request.application.is_some() || request.release.is_some() {
                return Err(invalid("value"));
            }
            let name = request.job.as_deref().ok_or_else(|| invalid("job"))?;
            return wamn_kubernetes::job::observe(
                api,
                name,
                &coordinate,
                &[
                    "deploy", "rollback", "stop", "diff", "apply", "reset", "retain",
                ],
            )
            .await
            .map_err(super::operation_refusal);
        }
        let operation =
            Operation::parse(attachment.route.operation).context("unknown operation Job route")?;
        validate(operation, &request)?;
        let environment: Value = sqlx::query_scalar("SELECT configuration FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3")
            .bind(self.org).bind(&request.project).bind(&request.env).fetch_one(self.control).await?;
        if let Some(application) = &request.application
            && environment["applications"].get(application).is_none()
        {
            return Err(invalid("application"));
        }
        let suffix: String = sqlx::query_scalar("SELECT instance_suffix FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3")
            .bind(self.org).bind(&request.project).bind(&request.env).fetch_one(self.control).await?;
        let database = wamn_control_provision::project_env_database_name(
            self.org,
            &request.project,
            &request.env,
            &suffix,
        );
        let worker = self
            .worker
            .context("installed operation worker is absent")?;
        let image = worker.image.as_str();
        let (_, digest) = image
            .rsplit_once("@sha256:")
            .context("operation image is not pinned")?;
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(anyhow::anyhow!("operation image digest is invalid").into());
        }
        let name = format!("op-{}", uuid::Uuid::new_v4());
        let mut job = manifest(
            operation,
            &request,
            self.org,
            caller.to_string().as_str(),
            &coordinate,
            &database,
            image,
            &name,
            &worker.configuration,
            &worker.database_credential,
            self.registry_token_metadata,
        );
        let administration = wamn_control_provision::control_administration_secret_name(self.org);
        // The connection projection selects the project's actual database server.
        job["spec"]["template"]["spec"]["volumes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"administration","configMap":{"name":administration}}));
        wamn_kubernetes::job::container(&mut job)["volumeMounts"]
            .as_array_mut()
            .unwrap()
            .push(
                json!({"name":"administration","mountPath":worker.administration,"readOnly":true}),
            );
        wamn_kubernetes::job::submit(api, &job)
            .await
            .map_err(Refusal::from)
    }
}

fn validate(operation: Operation, request: &Request) -> Result<(), Refusal> {
    if request.job.is_some() {
        return Err(invalid("job"));
    }
    if let Some(application) = &request.application
        && (application.is_empty()
            || application.len() > 253
            || application.contains(['\0', '\n', '\r']))
    {
        return Err(invalid("application"));
    }
    if let Some(release) = &request.release {
        let valid = release.strip_prefix("sha256:").is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        });
        if !valid {
            return Err(invalid("release"));
        }
    }
    match operation {
        Operation::Deploy if request.release.is_none() => {
            return Err(invalid("release"));
        }
        Operation::Deploy
        | Operation::Rollback
        | Operation::Stop
        | Operation::Apply
        | Operation::Diff
            if request.application.is_none() =>
        {
            return Err(invalid("application"));
        }
        Operation::Reset | Operation::Retain
            if request.application.is_some() || request.release.is_some() =>
        {
            return Err(invalid("application"));
        }
        _ => {}
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "all inputs are server-resolved manifest fields"
)]
fn manifest(
    operation: Operation,
    request: &Request,
    org: &str,
    actor: &str,
    coordinate: &str,
    database: &str,
    image: &str,
    name: &str,
    inputs: &str,
    apply_credential: &str,
    metadata: bool,
) -> Value {
    let mut arguments = vec![
        "--config",
        "/etc/wamn/control/config.json",
        "operation",
        operation.command(),
        "--org",
        org,
        "--project",
        request.project.as_str(),
        "--env",
        request.env.as_str(),
    ];
    for (flag, value) in [
        ("--application", request.application.as_deref()),
        ("--release", request.release.as_deref()),
    ] {
        if let Some(value) = value {
            arguments.extend([flag, value]);
        }
    }
    let mut volumes = vec![
        json!({"name":"inputs", "configMap":{"name":inputs}}),
        json!({"name":"work", "emptyDir":{}}),
    ];
    let mut mounts = vec![
        json!({"name":"inputs","mountPath":"/etc/wamn/control","readOnly":true}),
        json!({"name":"work","mountPath":"/work"}),
    ];
    for (secret, volume, path) in operation.needs(
        org,
        &request.project,
        &request.env,
        database,
        apply_credential,
    ) {
        volumes.push(json!({"name":volume,"secret":{"secretName":secret}}));
        mounts.push(json!({"name":volume,"mountPath":path,"readOnly":true}));
    }
    let mut job =
        wamn_kubernetes::job::manifest(name, image, &["/usr/local/bin/wamn-control"], &arguments);
    job["metadata"]["namespace"] = org.into();
    job["metadata"]["annotations"] = json!({TARGET:coordinate,OPERATION:operation.command()});
    job["spec"]["suspend"] = false.into();
    job["spec"]["ttlSecondsAfterFinished"] = 3600.into();
    // Reset changes the shared host chart under the existing lifecycle authority.
    job["spec"]["template"]["spec"]["serviceAccountName"] = match operation {
        Operation::Reset => format!("wamn-operation-{org}"),
        _ => format!(
            "{coordinate}-{}",
            if operation.privileged() {
                "sql"
            } else {
                "code"
            }
        ),
    }
    .into();
    job["spec"]["template"]["spec"]["automountServiceAccountToken"] = true.into();
    job["spec"]["template"]["spec"]["volumes"] = volumes.into();
    let container = wamn_kubernetes::job::container(&mut job);
    container["volumeMounts"] = mounts.into();
    container["resources"] =
        json!({"requests":{"cpu":"100m","memory":"256Mi"},"limits":{"cpu":"2","memory":"2Gi"}});
    wamn_kubernetes::job::registry(&mut job, metadata);
    wamn_kubernetes::job::environment(&mut job, &[("WAMN_DEPLOY_ACTOR", actor)]);
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_changes_require_one_application() {
        let mut request: Request = serde_json::from_value(json!({
            "project": "billing", "env": "dev"
        }))
        .unwrap();
        request.release = Some(wamn_digest::bytes_sha256(b"release"));
        for operation in [
            Operation::Deploy,
            Operation::Rollback,
            Operation::Apply,
            Operation::Diff,
        ] {
            assert!(validate(operation, &request).is_err());
            request.application = Some("billing".into());
            assert!(validate(operation, &request).is_ok());
            request.application = None;
        }
        request.release = None;
        assert!(validate(Operation::Reset, &request).is_ok());
        let coordinate = wamn_control_provision::environment_names::environment_resource_name(
            "acme",
            &request.project,
            &request.env,
        )
        .unwrap();
        for (operation, account) in [
            (Operation::Reset, "wamn-operation-acme".to_owned()),
            (Operation::Apply, format!("{coordinate}-sql")),
            (Operation::Diff, format!("{coordinate}-code")),
        ] {
            let job = manifest(
                operation,
                &request,
                "acme",
                "actor",
                &coordinate,
                "project-database",
                "image",
                "job",
                "inputs",
                "apply",
                false,
            );
            assert_eq!(
                job["spec"]["template"]["spec"]["serviceAccountName"],
                account
            );
        }
    }
}
