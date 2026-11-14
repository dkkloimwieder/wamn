//! Platform updates and SQL execute in the same native worker image.

use anyhow::{Context as _, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use wamn_control_provision::migrations::MigrationDatabase;
use wamn_identity_protocol::PrincipalId;

use crate::{Configuration, execution::Execution};

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Environment {
        operation: String,
        project: String,
        env: String,
        document: Option<Value>,
        application: Option<String>,
        disposable: Option<bool>,
        #[serde(default)]
        data: bool,
    },
    Update {
        document: Value,
        selection: String,
    },
    Schema {
        target: String,
        project: Option<String>,
        env: Option<String>,
        apply: bool,
    },
}

impl Execution {
    pub(crate) async fn platform(
        &self,
        operation: &str,
        actor: &PrincipalId,
        payload: &str,
    ) -> anyhow::Result<Value> {
        let api = wamn_kubernetes::kubernetes_api::KubernetesApi::in_cluster()?;
        ensure!(
            api.namespace == self.org,
            "platform worker must share the org namespace"
        );
        let environment = operation.starts_with("environment.");
        let input: Value = serde_json::from_str(payload)?;
        let coordinate = if environment {
            wamn_control_provision::environment_names::environment_resource_name(
                &self.org,
                input["project"].as_str().context("project is absent")?,
                input["env"].as_str().context("environment is absent")?,
            )?
        } else {
            format!("{}/platform", self.org)
        };
        let worker_type = if environment {
            "environment"
        } else {
            "platform"
        };
        if operation.ends_with("operation_status") {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Status {
                job: String,
                project: Option<String>,
                env: Option<String>,
            }
            let request: Status = serde_json::from_str(payload)?;
            let _ = (request.project, request.env);
            return wamn_kubernetes::job::observe(&api, &request.job, &coordinate, &[worker_type])
                .await;
        }
        let request: Request = serde_json::from_str(payload)?;
        let name = format!("op-{}", uuid::Uuid::new_v4());
        let worker = &self.worker;
        let mut job = wamn_kubernetes::job::manifest(
            &name,
            &worker.image,
            &["/usr/local/bin/wamn-control"],
            &[
                "--config",
                "/etc/wamn/control/config.json",
                "platform",
                payload,
            ],
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
            {"name":"work","emptyDir":{}},
            {"name":"administration","configMap":{"name":wamn_control_provision::control_administration_secret_name(&self.org)}}
        ]);
        wamn_kubernetes::job::container(&mut job)["volumeMounts"] = json!([
            {"name":"configuration","mountPath":"/etc/wamn/control","readOnly":true},
            {"name":"work","mountPath":"/work"},
            {"name":"administration","mountPath":self.administration,"readOnly":true}
        ]);
        let owner = if let Request::Schema {
            target,
            project,
            env,
            ..
        } = &request
            && target == "project"
        {
            let project = project.as_deref().context("project is absent")?;
            let env = env.as_deref().context("environment is absent")?;
            let triple = wamn_registry::Triple::new(&self.org, env, project);
            let instance = wamn_environment::provision_project_env::read_project_env_instance(
                &self.platform.system_database_url,
                &triple,
            )
            .await?;
            let database = wamn_control_provision::project_env_database_name(
                &self.org, project, env, &instance,
            );
            MigrationDatabase::Project.managed_role(&database)
        } else {
            MigrationDatabase::System.managed_role(&self.platform_database.database)
        };
        for (volume, secret, path) in [
            (
                "apply",
                self.platform_database.credentials.name.as_str(),
                "/credentials/apply",
            ),
            (
                "owner",
                owner.password_secret.name.as_str(),
                "/credentials/owner",
            ),
        ] {
            wamn_kubernetes::job::mount(&mut job, volume, secret, path, &["username", "password"]);
        }
        wamn_kubernetes::job::mount(
            &mut job,
            "nats",
            "wamn-ctl-nats",
            "/credentials/nats",
            &["ca.crt", "tls.crt", "tls.key"],
        );
        wamn_kubernetes::job::mount(
            &mut job,
            "identity-operator",
            &wamn_registry::name::resource_suffix("operator", &self.org),
            "/etc/identity-operator",
            &["tls.crt", "tls.key"],
        );
        job["spec"]["template"]["spec"]["volumes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"identity-ca","configMap":{"name":"identity-ca"}}));
        wamn_kubernetes::job::container(&mut job)["volumeMounts"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"identity-ca","mountPath":"/etc/identity-ca","readOnly":true}));
        wamn_kubernetes::job::registry(&mut job, self.platform.registry_token_metadata);
        let actor = actor.to_string();
        wamn_kubernetes::job::environment(&mut job, &[("WAMN_DEPLOY_ACTOR", &actor)]);
        wamn_kubernetes::job::submit(&api, &job).await
    }
}

/// Run a server-authorized update or platform SQL task inside its Job.
pub async fn execute(config: &Configuration, payload: &str) -> anyhow::Result<Value> {
    let actor: PrincipalId = std::env::var("WAMN_DEPLOY_ACTOR")
        .context("authenticated actor is absent")?
        .parse()?;
    let request: Request = serde_json::from_str(payload)?;
    let platform = config.platform()?;
    crate::write_kubeconfig(&platform.target.kubeconfig, &config.org)?;
    let mut output = Vec::new();
    match request {
        Request::Environment {
            operation,
            project,
            env,
            document,
            application,
            disposable,
            data,
        } => {
            let triple = wamn_registry::Triple::new(&config.org, env.as_str(), &project);
            let system =
                MigrationDatabase::System.configuration(&config.apply_database.to_json()?)?;
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
            let document = wamn_environment::environment::document::EnvironmentDocument::from_json(
                &serde_json::to_vec(&document.context("environment document is absent")?)?,
            )?;
            ensure!(
                document.triple() == triple,
                "environment document differs from its authorized coordinate"
            );
            wamn_environment::environment::lifecycle::configure(
                &platform,
                &document,
                application.as_deref(),
                disposable,
                &system,
                &actor.to_string(),
            )
            .await?;
            Ok(json!({"configured":true}))
        }
        Request::Update {
            document,
            selection,
        } => {
            let document = wamn_platform_install::platform::document::ClusterDocument::from_json(
                &serde_json::to_vec(&document)?,
            )?;
            ensure!(
                document.org == config.org,
                "platform update belongs to another org"
            );
            let selection = selection.parse()?;
            let chart = wamn_platform_install::platform::artifact_plan::select(&config.chart)?;
            wamn_platform_install::platform::deploy::update(
                &document,
                &chart,
                &platform.target,
                &selection,
                std::path::Path::new("/work"),
                &MigrationDatabase::System.configuration(&config.apply_database.to_json()?)?,
            )
            .await?;
            Ok(json!({"status":"updated"}))
        }
        Request::Schema {
            target,
            project,
            env,
            apply,
        } => {
            if target == "system" {
                ensure!(
                    project.is_none() && env.is_none(),
                    "system SQL has no project target"
                );
                let url =
                    MigrationDatabase::System.configuration(&config.apply_database.to_json()?)?;
                let mut client = wamn_postgres::connect(&url).await?;
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
                let project = project.context("project is absent")?;
                let env = env.context("environment is absent")?;
                let triple = wamn_registry::Triple::new(&config.org, env.as_str(), &project);
                let document = wamn_environment::environment::document::EnvironmentDocument::read(
                    &platform, &triple,
                )
                .await?;
                let instance = wamn_environment::provision_project_env::read_project_env_instance(
                    &platform.system_database_url,
                    &triple,
                )
                .await?;
                let database = wamn_control_provision::project_env_database_name(
                    &config.org,
                    &project,
                    &env,
                    &instance,
                );
                let key = wamn_control_provision::control_administration_key(&project, &env);
                let input = std::fs::read_to_string(config.administration_dir.join(key))?;
                ensure!(
                    wamn_postgres::identity(&input)?.1 == database,
                    "project SQL connection differs from the registry database"
                );
                let migration = MigrationDatabase::Project.configuration(&input)?;
                let expected = wamn_environment::environment::sql::ExpectedDatabase::read(
                    &platform, &document,
                )
                .await?;
                wamn_environment::environment::sql::project_schema(
                    &platform,
                    &document,
                    wamn_environment::environment::sql::ProjectSchemaRequest {
                        expected: &expected,
                        migration_database_url: &migration,
                        sources: &wamn_environment::platform_migrations::embedded_sources(
                            MigrationDatabase::Project,
                        ),
                        apply,
                    },
                    &mut output,
                )
                .await?;
            }
            Ok(json!({"log":String::from_utf8(output)?}))
        }
    }
}

impl Execution {
    pub(crate) async fn environment(
        &self,
        operation: &str,
        actor: &PrincipalId,
        payload: &str,
    ) -> anyhow::Result<Value> {
        if operation.ends_with("operation_status") {
            return self.platform(operation, actor, payload).await;
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            project: String,
            env: String,
            document: Option<Value>,
            application: Option<String>,
            disposable: Option<bool>,
            #[serde(default)]
            data: bool,
        }
        let input: Input = serde_json::from_str(payload)?;
        let payload = json!({"type":"environment","operation":operation,"project":input.project,"env":input.env,"document":input.document,"application":input.application,"disposable":input.disposable,"data":input.data});
        self.platform(operation, actor, &payload.to_string()).await
    }
}
