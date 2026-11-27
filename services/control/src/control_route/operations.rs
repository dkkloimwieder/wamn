//! Server-authorized application Jobs and bounded Kubernetes observations.

use anyhow::Context as _;
use serde_json::{Value, json};
pub(crate) use wamn_env::env::worker::{Operation, Request};
use wamn_identity_protocol::PrincipalId;
use wamn_kubernetes::client::KubernetesApi;
use wamn_routes::HostAttachment;

use super::{ControlRoutes, Refusal};

const TARGET: &str = "wamn.dev/operation-target";
const OPERATION: &str = "wamn.dev/operation";

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
        command: Option<&crate::platform_event::Command>,
    ) -> Result<Value, Refusal> {
        let request: Request = serde_json::from_str(payload).map_err(|_| invalid("value"))?;
        let coordinate = wamn_env::env_names::project_env_resource_name(
            self.org,
            &request.project,
            &request.env,
        )
        .map_err(anyhow::Error::from)?;
        let operation = if attachment.route.operation == "operation.status" {
            None
        } else {
            let operation = Operation::parse(attachment.route.operation)
                .context("unknown operation Job route")?;
            validate(operation, &request)?;
            Some(operation)
        };
        // Repeat authority in the write transaction before contacting Kubernetes.
        let mut writer = self.writer.lock().await?;
        let mut transaction = self
            .begin_in(&mut writer, attachment, caller, Some(&request.project))
            .await?;
        let triple = wamn_registry::Triple::new(
            self.org.clone(),
            request.env.clone(),
            request.project.clone(),
        );
        if operation.is_some()
            && wamn_env::registry::configuration::read(&mut transaction, &triple)
                .await?
                .is_none()
        {
            return Err(Refusal::Declared {
                code: "env_not_found",
                detail: json!({"field":"env"}),
            });
        }
        let namespace =
            wamn_env::registry::deployment::namespace(&mut transaction, self.org, &request.env)
                .await?
                .ok_or_else(|| Refusal::Declared {
                    code: "env_not_found",
                    detail: json!({"field":"env"}),
                })?;
        transaction.commit().await?;
        drop(writer);

        let mut api = if let Some(api) = self.operation_jobs {
            api.clone()
        } else {
            KubernetesApi::in_cluster().map_err(anyhow::Error::from)?
        };
        api.namespace = namespace;
        let Some(operation) = operation else {
            if request.app.is_some() || request.release.is_some() || request.revision.is_some() {
                return Err(invalid("value"));
            }
            let name = request.job.as_deref().ok_or_else(|| invalid("job"))?;
            let mut status = wamn_kubernetes::job::observe(
                &api,
                name,
                &coordinate,
                &[
                    "deploy", "rollback", "stop", "diff", "apply", "reset", "recover", "close",
                ],
            )
            .await
            .map_err(super::operation_refusal)?;
            if status["state"] == "failed"
                && let Ok(mut phase) = wamn_kubernetes::job::observe(
                    &api,
                    &format!("{name}-env"),
                    &coordinate,
                    &["project-env-phase"],
                )
                .await
                && let Value::String(log) = phase["log"].take()
                && !log.is_empty()
                && let Value::String(project_log) = &mut status["log"]
            {
                project_log.push_str("\nEnvironment phase\n");
                project_log.push_str(&log);
            }
            return Ok(status);
        };
        let worker = self
            .worker
            .context("installed operation worker is absent")?;
        let name = format!("op-{}", uuid::Uuid::new_v4());
        let mut job = manifest(
            &name,
            Some(operation),
            &triple,
            &api.namespace,
            &coordinate,
            worker,
            self.registry_token_metadata,
        );
        let mut input = json!({"type":"project_operation","operation":operation,"request":request});
        if operation.has_env_phase() {
            let env_name = format!("{name}-env");
            let mut phase_job = manifest(
                &env_name,
                None,
                &triple,
                &api.namespace,
                &coordinate,
                worker,
                self.registry_token_metadata,
            );
            let phase_task = json!({
                "type":"project_env_phase", "operation":operation,
                "project":request.project, "env":request.env, "project_job":name,
            });
            crate::platform::submit(&api, &mut phase_job, &phase_task, None).await?;
            input["jobs"] = json!({"project":name, "env":env_name});
        }
        crate::platform::submit(&api, &mut job, &input, command)
            .await
            .map_err(Refusal::from)
    }
}

fn validate(operation: Operation, request: &Request) -> Result<(), Refusal> {
    if request.job.is_some() {
        return Err(invalid("job"));
    }
    if request.revision.is_some() && !matches!(operation, Operation::Rollback) {
        return Err(invalid("revision"));
    }
    if request.release.is_some() && matches!(operation, Operation::Rollback) {
        return Err(invalid("release"));
    }
    match operation {
        Operation::Deploy | Operation::Apply if request.release.is_none() => {
            return Err(invalid("release"));
        }
        Operation::Deploy
        | Operation::Rollback
        | Operation::Stop
        | Operation::Apply
        | Operation::Diff
            if request.app.is_none() =>
        {
            return Err(invalid("app"));
        }
        Operation::Reset | Operation::Recover | Operation::Close
            if request.app.is_some() || request.release.is_some() =>
        {
            return Err(invalid("app"));
        }
        _ => {}
    }
    Ok(())
}

fn manifest(
    name: &str,
    operation: Option<Operation>,
    triple: &wamn_registry::Triple,
    namespace: &str,
    coordinate: &str,
    worker: &wamn_kubernetes::job::Worker,
    metadata: bool,
) -> Value {
    let mut job = wamn_kubernetes::job::manifest(
        name,
        &worker.image,
        &["/usr/local/bin/wamn-control"],
        &["--settings", crate::SETTINGS_FILE, "worker"],
    );
    job["metadata"]["namespace"] = namespace.into();
    let command = operation.map_or("project-env-phase", Operation::command);
    job["metadata"]["annotations"] = json!({TARGET:coordinate,OPERATION:command});
    job["spec"]["ttlSecondsAfterFinished"] = 3600.into();
    job["spec"]["template"]["spec"]["serviceAccountName"] = if operation.is_some() {
        format!("{coordinate}-operation").into()
    } else {
        worker.service_account.as_str().into()
    };
    job["spec"]["template"]["spec"]["automountServiceAccountToken"] = true.into();
    job["spec"]["template"]["spec"]["volumes"] = json!([
        {"name":"configuration","configMap":{"name":worker.configuration}},
        {"name":"work","emptyDir":{}}
    ]);
    let container = wamn_kubernetes::job::container(&mut job);
    container["volumeMounts"] = json!([
        {"name":"configuration","mountPath":"/etc/wamn/control","readOnly":true},
        {"name":"work","mountPath":"/work"}
    ]);
    container["resources"]["limits"]["cpu"] = "2".into();
    if operation.is_none() {
        // The env Job is submitted first and keeps its lock until the project Job exits.
        let deadline = job["spec"]["activeDeadlineSeconds"]
            .as_u64()
            .expect("Job deadline");
        job["spec"]["activeDeadlineSeconds"] =
            (deadline + wamn_kubernetes::client::REQUEST_TIMEOUT.as_secs() * 4).into();
    }
    if matches!(operation, Some(Operation::Recover)) {
        wamn_kubernetes::worker_mounts::mount_broker_ca(&mut job);
        wamn_kubernetes::job::mount(
            &mut job,
            "recovery-nats",
            &wamn_registry::name::project_nats_secret(triple, "recovery"),
            "/etc/wamn/recovery-nats",
            &[wamn_broker::nats::CREDENTIALS_KEY],
        );
    }
    wamn_platform_install::worker_mounts::mount_database_files(&mut job);
    wamn_kubernetes::worker_mounts::mount_registry(&mut job, metadata);
    job
}

#[cfg(test)]
mod tests {
    use super::{Operation, manifest};
    use wamn_kubernetes::job::Worker;
    use wamn_postgres::database::DATABASE_DIR;
    use wamn_registry::Triple;

    #[test]
    fn the_environment_phase_runs_the_companion_on_the_env_account() -> anyhow::Result<()> {
        let triple = Triple::new("acme".parse()?, "dev".parse()?, "billing".parse()?);
        let namespace = wamn_registry::name::env_key(&triple.org, &triple.env);
        let coordinate = wamn_registry::name::project_env_resource_name(
            &triple.org,
            &triple.project,
            &triple.env,
        );
        let worker = Worker {
            image: "registry.example/control".into(),
            configuration: "control-settings".into(),
            service_account: wamn_platform_install::env_install::ENV_ACCOUNT.into(),
        };
        let project = manifest(
            "op-example",
            Some(Operation::Reset),
            &triple,
            &namespace,
            &coordinate,
            &worker,
            false,
        );
        let companion = manifest(
            "op-example-env",
            None,
            &triple,
            &namespace,
            &coordinate,
            &worker,
            false,
        );
        let account = |job: &serde_json::Value| {
            job["spec"]["template"]["spec"]["serviceAccountName"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(account(&project), format!("{coordinate}-operation"));
        assert_eq!(account(&companion), worker.service_account);
        assert_eq!(
            project["metadata"]["annotations"][super::TARGET],
            companion["metadata"]["annotations"][super::TARGET]
        );
        assert_ne!(
            project["metadata"]["annotations"][super::OPERATION],
            companion["metadata"]["annotations"][super::OPERATION]
        );
        Ok(())
    }

    #[test]
    fn operation_job_keeps_work_and_database_staging_writable() -> anyhow::Result<()> {
        let triple = Triple::new("acme".parse()?, "dev".parse()?, "billing".parse()?);
        let worker = Worker {
            image: "registry.example/control".into(),
            configuration: "control-settings".into(),
            service_account: "worker".into(),
        };
        let namespace = wamn_registry::name::env_key(&triple.org, &triple.env);
        let job = manifest(
            "op-test",
            Some(Operation::Deploy),
            &triple,
            &namespace,
            "billing-dev",
            &worker,
            false,
        );
        assert_eq!(job["metadata"]["namespace"], namespace);
        let pod = &job["spec"]["template"]["spec"];
        let mounts = pod["containers"][0]["volumeMounts"]
            .as_array()
            .expect("worker mounts");
        let volumes = pod["volumes"].as_array().expect("worker volumes");
        for (path, medium) in [("/work", None), (DATABASE_DIR, Some("Memory"))] {
            let mount = mounts
                .iter()
                .find(|mount| mount["mountPath"] == path)
                .unwrap_or_else(|| panic!("missing {path} mount"));
            assert!(!mount["readOnly"].as_bool().unwrap_or(false), "{path}");
            let volume = volumes
                .iter()
                .find(|volume| volume["name"] == mount["name"])
                .unwrap_or_else(|| panic!("missing {path} volume"));
            assert!(volume["emptyDir"].is_object(), "{path} needs an emptyDir");
            assert_eq!(volume["emptyDir"]["medium"].as_str(), medium, "{path}");
        }
        Ok(())
    }

    #[test]
    fn project_operation_jobs_keep_their_cpu_limit() -> anyhow::Result<()> {
        let triple = Triple::new("acme".parse()?, "dev".parse()?, "billing".parse()?);
        let worker = Worker {
            image: "registry.example/control".into(),
            configuration: "control-settings".into(),
            service_account: "worker".into(),
        };
        let namespace = wamn_registry::name::env_key(&triple.org, &triple.env);
        let job = manifest(
            "op-test",
            Some(Operation::Deploy),
            &triple,
            &namespace,
            "billing-dev",
            &worker,
            false,
        );
        let containers = job["spec"]["template"]["spec"]["containers"]
            .as_array()
            .expect("project Job containers");
        assert!(!containers.is_empty());
        for container in containers {
            assert_eq!(container["resources"]["limits"]["cpu"], "2");
        }
        Ok(())
    }
}
