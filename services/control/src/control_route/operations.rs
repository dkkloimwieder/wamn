//! Server-authorized application Jobs and bounded Kubernetes observations.

use wamn_routes::HostAttachment;
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wamn_identity_protocol::PrincipalId;
use wamn_kubernetes::kubernetes_api::KubernetesApi;

use super::{ControlRoutes, Refusal};

const TARGET: &str = "wamn.dev/operation-target";
const OPERATION: &str = "wamn.dev/operation";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub(crate) project: wamn_registry::ProjectId,
    pub(crate) env: wamn_registry::EnvName,
    #[serde(default)]
    pub(crate) application: Option<wamn_registry::ApplicationName>,
    #[serde(default)]
    pub(crate) release: Option<wamn_digest::Digest>,
    #[serde(default)]
    job: Option<String>,
    pub(crate) revision: Option<std::num::NonZeroU32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Operation {
    Deploy,
    Rollback,
    Stop,
    Diff,
    Apply,
    Reset,
    Recover,
    Close,
}

impl Operation {
    fn parse(operation: &str) -> Option<Self> {
        Some(match operation {
            "application.deploy" => Self::Deploy,
            "application.rollback" => Self::Rollback,
            "application.stop" => Self::Stop,
            "schema.diff" => Self::Diff,
            "schema.apply" => Self::Apply,
            "schema.reset" => Self::Reset,
            "capture_gap.recover" => Self::Recover,
            "capture_gap.close" => Self::Close,
            _ => return None,
        })
    }

    fn command(self) -> &'static str {
        match self {
            Self::Deploy => "deploy",
            Self::Rollback => "rollback",
            Self::Stop => "stop",
            Self::Diff => "diff",
            Self::Apply => "apply",
            Self::Reset => "reset",
            Self::Recover => "recover",
            Self::Close => "close",
        }
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
        let coordinate = wamn_provision::environment_names::environment_resource_name(
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
            && wamn_registry::configuration::read(&mut transaction, &triple)
                .await?
                .is_none()
        {
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
        let Some(operation) = operation else {
            if request.application.is_some()
                || request.release.is_some()
                || request.revision.is_some()
            {
                return Err(invalid("value"));
            }
            let name = request.job.as_deref().ok_or_else(|| invalid("job"))?;
            return wamn_kubernetes::job::observe(
                api,
                name,
                &coordinate,
                &[
                    "deploy", "rollback", "stop", "diff", "apply", "reset", "recover", "close",
                ],
            )
            .await
            .map_err(super::operation_refusal);
        };
        let worker = self
            .worker
            .context("installed operation worker is absent")?;
        let mut job = manifest(
            operation,
            &triple,
            &coordinate,
            worker,
            self.registry_token_metadata,
        );
        let input = json!({"type":"project_operation","operation":operation,"request":request});
        wamn_kubernetes::job::submit(api, &mut job, &input.to_string())
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
            if request.application.is_none() =>
        {
            return Err(invalid("application"));
        }
        Operation::Reset | Operation::Recover | Operation::Close
            if request.application.is_some() || request.release.is_some() =>
        {
            return Err(invalid("application"));
        }
        _ => {}
    }
    Ok(())
}

fn manifest(
    operation: Operation,
    triple: &wamn_registry::Triple,
    coordinate: &str,
    worker: &wamn_kubernetes::job::Worker,
    metadata: bool,
) -> Value {
    let name = format!("op-{}", uuid::Uuid::new_v4());
    let mut job = wamn_kubernetes::job::manifest(
        &name,
        &worker.image,
        &["/usr/local/bin/wamn-control"],
        &["--settings", crate::SETTINGS_FILE, "worker"],
    );
    job["metadata"]["namespace"] = triple.org.as_str().into();
    job["metadata"]["annotations"] = json!({TARGET:coordinate,OPERATION:operation.command()});
    job["spec"]["ttlSecondsAfterFinished"] = 3600.into();
    job["spec"]["template"]["spec"]["serviceAccountName"] = match operation {
        Operation::Diff => format!("{coordinate}-operation").into(),
        Operation::Deploy
        | Operation::Rollback
        | Operation::Stop
        | Operation::Apply
        | Operation::Reset
        | Operation::Recover
        | Operation::Close => worker.service_account.as_str().into(),
    };
    job["spec"]["template"]["spec"]["automountServiceAccountToken"] = true.into();
    job["spec"]["template"]["spec"]["volumes"] = json!([
        {"name":"configuration","configMap":{"name":worker.configuration}},
        {"name":"environment","configMap":{"name":format!("{coordinate}-operations")}},
        {"name":"work","emptyDir":{}}
    ]);
    let container = wamn_kubernetes::job::container(&mut job);
    container["volumeMounts"] = json!([
        {"name":"configuration","mountPath":"/etc/wamn/control","readOnly":true},
        {"name":"environment","mountPath":"/etc/wamn/environment","readOnly":true},
        {"name":"work","mountPath":"/work"}
    ]);
    container["resources"] =
        json!({"requests":{"cpu":"100m","memory":"256Mi"},"limits":{"cpu":"2","memory":"2Gi"}});
    if matches!(operation, Operation::Recover) {
        crate::worker::mount_broker_ca(&mut job);
        wamn_kubernetes::job::mount(
            &mut job,
            "recovery-nats",
            &wamn_registry::name::project_nats_secret(triple, "recovery"),
            "/etc/wamn/recovery-nats",
            &[wamn_broker::nats::CREDENTIALS_KEY],
        );
    }
    crate::worker::mount_database_files(&mut job);
    crate::worker::mount_registry(&mut job, metadata);
    job
}
