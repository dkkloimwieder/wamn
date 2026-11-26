//! Submit and run platform SQL, environment, and project operation Jobs.

use anyhow::{Context as _, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use wamn_env::env::worker::{self, Task};
use wamn_kubernetes::client::KubernetesApi;
use wamn_kubernetes::helm::UpgradeRolledBack;

use crate::{
    Configuration,
    execution::Execution,
    platform_event::{self, Command, Outcome},
};

impl Execution {
    pub(crate) async fn platform_schema(
        &self,
        operation: &wamn_package::canonical_name::CanonicalName,
        payload: &str,
        command: Option<&Command>,
    ) -> anyhow::Result<Value> {
        if operation.as_str() == "platform.operation_status" {
            return self.operation_job(operation, payload, command).await;
        }
        let mut input: Value = serde_json::from_str(payload)?;
        let fields = input
            .as_object_mut()
            .context("expected platform SQL object")?;
        ensure!(
            fields.get("target").and_then(Value::as_str) == Some("project"),
            "platform SQL through control requires the project target"
        );
        ensure!(
            fields.insert("type".into(), "schema".into()).is_none(),
            "platform SQL request cannot select a worker task"
        );
        self.operation_job(operation, &input.to_string(), command)
            .await
    }

    async fn operation_job(
        &self,
        operation: &wamn_package::canonical_name::CanonicalName,
        payload: &str,
        command: Option<&Command>,
    ) -> anyhow::Result<Value> {
        let api = wamn_kubernetes::client::KubernetesApi::in_cluster()?;
        ensure!(
            api.namespace == self.org,
            "platform worker must share the org namespace"
        );
        let env = operation.as_str().starts_with("environment.");
        let input: Value = serde_json::from_str(payload)?;
        let coordinate = if env {
            wamn_env::env_names::project_env_resource_name(
                &self.org,
                &input["project"]
                    .as_str()
                    .context("project is absent")?
                    .parse()?,
                &input["env"]
                    .as_str()
                    .context("environment is absent")?
                    .parse()?,
            )?
        } else {
            format!("{}/platform", self.org)
        };
        let worker_type = if env { "environment" } else { "platform" };
        if operation.as_str().ends_with("operation_status") {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Status {
                job: String,
                project: Option<wamn_registry::ProjectId>,
                env: Option<wamn_registry::EnvName>,
            }
            let request: Status = serde_json::from_str(payload)?;
            let _ = (request.project, request.env);
            return wamn_kubernetes::job::observe(&api, &request.job, &coordinate, &[worker_type])
                .await;
        }
        ensure!(
            !matches!(
                serde_json::from_str::<Task>(payload)?,
                Task::ProjectOperation { .. }
            ),
            "project operations require project route authorization"
        );
        let name = format!("op-{}", uuid::Uuid::new_v4());
        let worker = &self.worker;
        let mut job = wamn_kubernetes::job::manifest(
            &name,
            &worker.image,
            &["/usr/local/bin/wamn-control"],
            &["--settings", crate::SETTINGS_FILE, "worker"],
        );
        job["metadata"]["annotations"] =
            json!({"wamn.dev/operation-target":coordinate,"wamn.dev/operation":worker_type});
        job["spec"]["suspend"] = false.into();
        job["spec"]["ttlSecondsAfterFinished"] = 3600.into();
        job["spec"]["template"]["spec"]["serviceAccountName"] =
            worker.service_account.clone().into();
        job["spec"]["template"]["spec"]["automountServiceAccountToken"] = true.into();
        job["spec"]["template"]["spec"]["volumes"] = json!([
            {"name":"configuration","configMap":{"name":worker.configuration}},
            {"name":"work","emptyDir":{}}
        ]);
        wamn_kubernetes::job::container(&mut job)["volumeMounts"] = json!([
            {"name":"configuration","mountPath":"/etc/wamn/control","readOnly":true},
            {"name":"work","mountPath":"/work"}
        ]);
        if env {
            wamn_kubernetes::worker_mounts::mount_broker_ca(&mut job);
            wamn_kubernetes::worker_mounts::mount_identity_worker_token(
                &mut job,
                std::path::Path::new(wamn_identity_client::TOKEN_PATH),
                wamn_identity_client::TOKEN_AUDIENCE,
            );
        }
        wamn_platform_install::worker_mounts::mount_database_files(&mut job);
        wamn_kubernetes::worker_mounts::mount_registry(&mut job, self.registry_token_metadata);
        submit(&api, &mut job, &input, command).await
    }
}

pub(crate) async fn submit(
    api: &KubernetesApi,
    job: &mut Value,
    task: &Value,
    command: Option<&Command>,
) -> anyhow::Result<Value> {
    let input = match command {
        Some(command) => json!({
            "task": task,
            "command": command,
            "job_namespace": api.namespace,
            "job_name": job["metadata"]["name"]
        })
        .to_string(),
        None => task.to_string(),
    };
    wamn_kubernetes::job::submit(api, job, &input).await
}

/// Run a server-authorized lifecycle operation or platform SQL task in its Job.
pub async fn execute(config: &Configuration, payload: &str) -> anyhow::Result<Value> {
    #[derive(Deserialize)]
    #[serde(untagged, deny_unknown_fields)]
    enum Request {
        Journaled {
            task: Task,
            command: Command,
            job_namespace: String,
            job_name: String,
        },
        Plain(Task),
    }

    let (task, journal) = match serde_json::from_str(payload)? {
        Request::Journaled {
            task,
            command,
            job_namespace,
            job_name,
        } => (task, Some((command, job_namespace, job_name))),
        Request::Plain(task) => (task, None),
    };
    let files = wamn_platform_install::database_files::DatabaseFiles::in_cluster()?;
    files.materialize(&config.apply_database).await?;
    let mut release = None;
    let result = async {
        let mut platform = config.platform()?;
        platform.database_files = Some(std::sync::Arc::new(files));
        crate::write_kubeconfig(
            std::path::Path::new(crate::WORKER_KUBECONFIG),
            config.org.as_str(),
        )?;
        worker::execute_task(
            platform,
            &config.org,
            &config.registry,
            &config.platform_registry,
            task,
            wamn_http::wasm_authority::wasm_authority_port,
            &mut release,
        )
        .await
    }
    .await;
    if let Some((mut command, job_namespace, job_name)) = journal {
        if let Some(release) = release {
            command.package_digest = Some(release.package_digest);
            command.helm_revision = i32::try_from(release.revision).ok();
        }
        let outcome = worker_outcome(&result);
        let recorded = async {
            let mut connection = wamn_postgres::connect(&config.apply_database.to_json()?).await?;
            platform_event::record(
                &mut connection,
                &config.org,
                &command,
                outcome,
                Some((&job_namespace, &job_name)),
            )
            .await
        }
        .await;
        if result.is_ok() {
            recorded?;
        } else if let Err(error) = recorded {
            tracing::error!(%error, command_id = %command.id, "record the worker command outcome");
        }
    }
    result
}

fn worker_outcome(result: &anyhow::Result<Value>) -> Outcome {
    match result {
        Err(error) if error.downcast_ref::<UpgradeRolledBack>().is_some() => Outcome::RolledBack,
        Err(_) => Outcome::Failed("The lifecycle operation Job failed."),
        Ok(_) => Outcome::Succeeded,
    }
}

#[cfg(test)]
#[test]
fn worker_outcome_records_a_restored_upgrade_as_rolled_back() {
    let source = anyhow::anyhow!("upgrade failed");
    let restored = UpgradeRolledBack::new("wamn-helm-example", "shop", 2, source);
    let result = Err(anyhow::Error::new(restored).context("deploy application"));
    assert!(matches!(worker_outcome(&result), Outcome::RolledBack));

    let failed = Err(anyhow::anyhow!("restore failed"));
    assert!(matches!(worker_outcome(&failed), Outcome::Failed(_)));
}

impl Execution {
    pub(crate) async fn env(
        &self,
        operation: &wamn_package::canonical_name::CanonicalName,
        payload: &str,
        command: Option<&Command>,
    ) -> anyhow::Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            project: wamn_registry::ProjectId,
            env: wamn_registry::EnvName,
            document: Option<Value>,
            application: Option<wamn_registry::AppName>,
            disposable: Option<bool>,
            #[serde(default)]
            data: bool,
        }
        if operation.as_str().ends_with("operation_status") {
            return self.operation_job(operation, payload, command).await;
        }
        let input: Input = serde_json::from_str(payload)?;
        let payload = json!({"type":"environment","operation":operation,"project":input.project,"env":input.env,"document":input.document,"application":input.application,"disposable":input.disposable,"data":input.data});
        self.operation_job(operation, &payload.to_string(), command)
            .await
    }
}
