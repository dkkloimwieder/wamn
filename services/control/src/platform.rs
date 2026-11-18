//! Platform updates and SQL execute in the same native worker image.

use anyhow::{Context as _, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use wamn_catalog::lifecycle_lock::LifecycleLock;
use wamn_environment::environment::document::{EnvironmentDocument, EnvironmentFile};
use wamn_identity_protocol::PrincipalId;
use wamn_provision::migrations::MigrationDatabase;

use crate::{Configuration, execution::Execution};

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
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
    Update {
        document: Value,
        selection: String,
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
    pub(crate) async fn platform(
        &self,
        operation: &wamn_catalog::canonical_name::CanonicalName,
        actor: &PrincipalId,
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
            {"name":"administration","configMap":{"name":wamn_provision::control_administration_secret_name(&self.org)}}
        ]);
        wamn_kubernetes::job::container(&mut job)["volumeMounts"] = json!([
            {"name":"configuration","mountPath":"/etc/wamn/control","readOnly":true},
            {"name":"work","mountPath":"/work"},
            {"name":"administration","mountPath":self.administration,"readOnly":true}
        ]);
        wamn_kubernetes::job::mount(
            &mut job,
            "apply",
            self.platform_database.credentials.name.as_str(),
            "/credentials/apply",
            &["username", "password"],
        );
        // The project SQL path reads the named login through its existing API access.
        if !matches!(&request, Request::Schema { target, .. } if target == "project") {
            let owner = MigrationDatabase::System.managed_role(&self.platform_database.database);
            wamn_kubernetes::job::mount(
                &mut job,
                "owner",
                &owner.password_secret.name,
                "/credentials/owner",
                &["username", "password"],
            );
        }
        wamn_kubernetes::job::mount(
            &mut job,
            "identity-operator",
            &wamn_registry::name::resource_suffix("operator", self.org.as_str()),
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
    let request: Request = serde_json::from_str(payload)?;
    let platform = config.platform()?;
    crate::write_kubeconfig(&platform.target.kubeconfig, config.org.as_str())?;
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
            let triple =
                wamn_registry::Triple::new(config.org.clone(), env.clone(), project.clone());
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
                                config,
                                &platform,
                                &lock,
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
    config: &Configuration,
    platform: &wamn_environment::environment::Platform,
    lock: &LifecycleLock,
    triple: &wamn_registry::Triple,
    sources: &[sqlx::migrate::Migration],
    apply: bool,
    output: &mut Vec<u8>,
) -> anyhow::Result<()> {
    use std::io::Write as _;

    EnvironmentDocument::read(platform, triple).await?;
    let instance = wamn_registry::deployment::instance_suffix(&mut *lock.client().await, triple)
        .await?
        .context("project deployment is absent")?;
    let database = wamn_provision::project_env_database_name(
        &triple.org,
        &triple.project,
        &triple.env,
        &instance,
    );
    let key = wamn_provision::control_administration_key(&triple.project, &triple.env);
    let input = std::fs::read_to_string(config.administration_dir.join(key))?;
    ensure!(
        wamn_postgres::identity(&input)?.1 == database,
        "project SQL connection differs from the registry database"
    );
    let migration = MigrationDatabase::Project.configuration(&input)?;
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
        actor: &PrincipalId,
        payload: &str,
    ) -> anyhow::Result<Value> {
        if operation.as_str().ends_with("operation_status") {
            return self.platform(operation, actor, payload).await;
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
        self.platform(operation, actor, &payload.to_string()).await
    }
}
