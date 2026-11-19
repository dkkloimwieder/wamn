//! Submit and run platform SQL, environment, and project operation Jobs.

use anyhow::{Context as _, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use wamn_catalog::lifecycle_lock::LifecycleLock;
use wamn_environment::environment::document::EnvironmentFile;
use wamn_identity_protocol::PrincipalId;
use wamn_provision::migrations::MigrationDatabase;

use crate::{Configuration, execution::Execution};

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    ProjectOperation {
        operation: crate::control_route::operations::Operation,
        request: crate::control_route::operations::Request,
        actor: PrincipalId,
    },
    Environment {
        operation: String,
        project: wamn_registry::ProjectId,
        env: wamn_registry::EnvName,
        document: Option<Value>,
        application: Option<wamn_catalog::ApplicationName>,
        disposable: Option<bool>,
        #[serde(default)]
        data: bool,
    },
    Schema {
        target: String,
        #[serde(default)]
        projects: Vec<wamn_registry::ProjectId>,
        org: Option<wamn_registry::OrgId>,
        env: Option<wamn_registry::EnvName>,
        apply: bool,
    },
}

impl Execution {
    pub(crate) async fn platform_schema(
        &self,
        operation: &wamn_catalog::canonical_name::CanonicalName,
        payload: &str,
    ) -> anyhow::Result<Value> {
        if operation.as_str() == "platform.operation_status" {
            return self.operation_job(operation, payload).await;
        }
        let mut input: Value = serde_json::from_str(payload)?;
        let fields = input.as_object_mut().context("expected platform SQL object")?;
        ensure!(
            fields.insert("type".into(), "schema".into()).is_none(),
            "platform SQL request cannot select a worker task"
        );
        self.operation_job(operation, &input.to_string()).await
    }

    async fn operation_job(
        &self,
        operation: &wamn_catalog::canonical_name::CanonicalName,
        payload: &str,
    ) -> anyhow::Result<Value> {
        let api = wamn_kubernetes::kubernetes_api::KubernetesApi::in_cluster()?;
        ensure!(
            api.namespace == self.org,
            "platform worker must share the org namespace"
        );
        let environment = operation.as_str().starts_with("environment.");
        let input: Value = serde_json::from_str(payload)?;
        let coordinate = if environment {
            wamn_provision::environment_names::environment_resource_name(
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
        let worker_type = if environment {
            "environment"
        } else {
            "platform"
        };
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
        let admin = match serde_json::from_str::<Request>(payload)? {
            Request::ProjectOperation { .. } => {
                anyhow::bail!("project operations require project route authorization")
            }
            Request::Environment { operation, env, .. } if operation == "environment.delete" => {
                Some(wamn_registry::name::environment_nats_admin(&self.org, &env))
            }
            _ => None,
        };
        let name = format!("op-{}", uuid::Uuid::new_v4());
        let worker = &self.worker;
        let mut job = wamn_kubernetes::job::manifest(
            &name,
            &worker.image,
            &["/usr/local/bin/wamn-control"],
            &["--config", "/etc/wamn/control/config.json", "worker"],
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
        if let Some(admin) = admin {
            wamn_kubernetes::job::mount(
                &mut job,
                "nats-admin",
                &admin,
                wamn_broker::authorization::ADMIN_CREDENTIAL_DIR,
                &["ca.crt", "tls.crt", "tls.key"],
            );
            // Delete can resume after the account and its credential are gone.
            let volume = job["spec"]["template"]["spec"]["volumes"]
                .as_array_mut()
                .and_then(|volumes| volumes.last_mut())
                .expect("mount appends a volume");
            volume["secret"]["optional"] = true.into();
        }
        crate::worker::mount_database_files(&mut job);
        crate::worker::mount_registry(&mut job, self.registry_token_metadata);
        wamn_kubernetes::job::submit(&api, &mut job, payload).await
    }
}

/// Run a server-authorized lifecycle operation or platform SQL task in its Job.
pub async fn execute(config: &Configuration, payload: &str) -> anyhow::Result<Value> {
    let request: Request = serde_json::from_str(payload)?;
    let mut platform = config.platform()?;
    let files = wamn_platform_install::credentials::DatabaseFiles::in_cluster()?;
    let system = MigrationDatabase::System.configuration(&platform.system_database_url)?;
    files.materialize(&serde_json::from_str(&system)?).await?;
    if matches!(&request, Request::Environment { .. })
        || matches!(&request, Request::Schema { target, .. } if target == "project")
    {
        files.materialize(&config.apply_database).await?;
    }
    platform.database_files = Some(files);
    crate::write_kubeconfig(&platform.target.kubeconfig, config.org.as_str())?;
    let mut output = Vec::new();
    match request {
        Request::ProjectOperation {
            operation,
            request,
            actor,
        } => {
            platform.system_database_url = system;
            crate::worker::execute(config, platform, operation, request, &actor).await
        }
        Request::Environment {
            operation,
            project,
            env,
            document,
            application,
            disposable,
            data,
        } => {
            let triple =
                wamn_registry::Triple::new(config.org.clone(), env.clone(), project.clone());
            if operation == "environment.delete" {
                let messages = wamn_environment::environment::delete::delete(
                    &platform, &triple, data, &system,
                )
                .await?;
                return Ok(json!({"messages":messages}));
            }
            ensure!(
                operation == "environment.configure",
                "unknown environment lifecycle operation"
            );
            let document: EnvironmentFile =
                serde_json::from_value(document.context("environment document is absent")?)?;
            document.require_valid()?;
            let selected = match &application {
                Some(application) => document.application_project(application)?,
                None => &document.projects[0].name,
            };
            ensure!(
                &triple.project == selected
                    && triple.org == document.org
                    && triple.env == document.env,
                "environment document differs from its authorized coordinate"
            );
            wamn_environment::environment::lifecycle::configure(
                &platform,
                document,
                application.as_ref(),
                disposable,
                &system,
                config.max_projects,
            )
            .await?;
            Ok(json!({"configured":true}))
        }
        Request::Schema {
            target,
            projects,
            org,
            env,
            apply,
        } => {
            if target == "system" {
                ensure!(
                    projects.is_empty() && org.is_none() && env.is_none(),
                    "system SQL has no project target"
                );
                let mut client = wamn_postgres::connect(&system).await?;
                wamn_environment::platform_migrations::run(
                    &mut client,
                    wamn_environment::platform_migrations::MigrationTarget::System,
                    &wamn_environment::platform_migrations::embedded_sources(
                        MigrationDatabase::System,
                    ),
                    apply,
                    &mut output,
                )
                .await?;
            } else {
                ensure!(target == "project", "unknown platform SQL target");
                let env = env.context("environment is absent")?;
                ensure!(
                    org.as_ref() == Some(&config.org),
                    "environment belongs to another org"
                );
                let first = projects.first().context("environment has no projects")?;
                let coordinate =
                    wamn_registry::Triple::new(config.org.clone(), env.clone(), first.clone());
                let lock = LifecycleLock::acquire_wait(
                    wamn_postgres::connect(&platform.system_database_url).await?,
                    &coordinate,
                )
                .await?;
                let result = lock
                    .protect(async {
                        let sources = wamn_environment::platform_migrations::embedded_sources(
                            MigrationDatabase::Project,
                        );
                        for project in projects {
                            let triple = wamn_registry::Triple::new(
                                config.org.clone(),
                                env.clone(),
                                project,
                            );
                            project_schema(
                                &platform,
                                &lock,
                                &system,
                                &triple,
                                &sources,
                                apply,
                                &mut output,
                            )
                            .await
                            .with_context(|| format!("platform SQL for {}", triple.project))?;
                        }
                        Ok(())
                    })
                    .await;
                lock.release().await;
                result?;
            }
            Ok(json!({"log":String::from_utf8(output)?}))
        }
    }
}

async fn project_schema(
    platform: &wamn_environment::environment::Platform,
    lock: &LifecycleLock,
    system: &str,
    triple: &wamn_registry::Triple,
    sources: &[sqlx::migrate::Migration],
    apply: bool,
    output: &mut Vec<u8>,
) -> anyhow::Result<()> {
    use std::io::Write as _;

    let migration = wamn_platform_install::environment::connection(system, triple).await?;
    platform.prepare_database(&migration).await?;
    let migration = migration.to_json()?;
    writeln!(output, "Project {}", triple.project)?;
    wamn_environment::environment::sql::project_schema(
        lock, triple, &migration, sources, apply, output,
    )
    .await
}

impl Execution {
    pub(crate) async fn environment(
        &self,
        operation: &wamn_catalog::canonical_name::CanonicalName,
        payload: &str,
    ) -> anyhow::Result<Value> {
        if operation.as_str().ends_with("operation_status") {
            return self.operation_job(operation, payload).await;
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            project: wamn_registry::ProjectId,
            env: wamn_registry::EnvName,
            document: Option<Value>,
            application: Option<wamn_catalog::ApplicationName>,
            disposable: Option<bool>,
            #[serde(default)]
            data: bool,
        }
        let input: Input = serde_json::from_str(payload)?;
        let payload = json!({"type":"environment","operation":operation,"project":input.project,"env":input.env,"document":input.document,"application":input.application,"disposable":input.disposable,"data":input.data});
        self.operation_job(operation, &payload.to_string()).await
    }
}
