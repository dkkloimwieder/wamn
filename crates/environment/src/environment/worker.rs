//! Execute a project operation against the mounted environment document.

use super::{Platform, capture_gap, document::EnvironmentFile, operation};
use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use wamn_registry::{OrgId, Triple};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub project: wamn_registry::ProjectId,
    pub env: wamn_registry::EnvName,
    #[serde(default)]
    pub application: Option<wamn_registry::ApplicationName>,
    #[serde(default)]
    pub release: Option<wamn_digest::Digest>,
    #[serde(default)]
    pub job: Option<String>,
    pub revision: Option<std::num::NonZeroU32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
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
    pub fn parse(operation: &str) -> Option<Self> {
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

    pub fn command(self) -> &'static str {
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

pub async fn execute(
    org: &OrgId,
    registry: &str,
    platform: Platform,
    operation: Operation,
    request: Request,
    authority_port: super::write::AuthorityPort,
) -> anyhow::Result<Value> {
    let file = EnvironmentFile::compile(Path::new("/etc/wamn/environment/environment.yaml"))?;
    ensure!(
        &file.org == org && file.env == request.env,
        "mounted environment does not match the Job target"
    );
    let triple = Triple::new(file.org.clone(), request.env, request.project);
    let chart = (platform.select_chart)(&platform.chart)?;
    let platform = Platform {
        chart: chart.directory().to_owned(),
        ..platform
    };
    match operation {
        Operation::Recover => {
            let recovered = capture_gap::recover(
                &platform,
                &capture_gap::RecoverCaptureGapRequest {
                    org: triple.org,
                    project: triple.project,
                    env: triple.env,
                    system_database_url: platform.system_database_url.clone(),
                    nats: wamn_event::deploy_broker::EventObserver {
                        nats_url: platform.nats_url.clone(),
                        nats_tls_ca: wamn_event::deploy_broker::CA_FILE.into(),
                        creds: "/etc/wamn/recovery-nats/user.creds".into(),
                    },
                },
            )
            .await?;
            Ok(
                json!({"slot":recovered.replication_name,"reason":recovered.reason,"start_lsn":recovered.start_lsn,
                "start_at":recovered.start_at.to_rfc3339(),"end_lsn":recovered.end_lsn,
                "created_at":recovered.created_at.to_rfc3339()}),
            )
        }
        Operation::Close => {
            let closed = capture_gap::close(
                &platform,
                &capture_gap::CloseCaptureGapRequest {
                    org: triple.org,
                    project: triple.project,
                    env: triple.env,
                    system_database_url: platform.system_database_url.clone(),
                },
            )
            .await?;
            Ok(
                json!({"slot":closed.replication_name,"created_at":closed.created_at.to_rfc3339(),
                "resync_at":closed.resync_at.to_rfc3339()}),
            )
        }
        Operation::Reset => operation::reset(&platform, &file.into_project(&triple.project)?).await,
        _ => {
            let application = request.application.context("application is required")?;
            let document = file.into_project(&triple.project)?;
            let integration_base;
            let component_base;
            operation::application(
                &platform,
                document,
                &application,
                match operation {
                    Operation::Deploy => {
                        integration_base = format!("{registry}/wamn/integrations");
                        component_base = format!("{registry}/wamn/components");
                        operation::Application::Deploy {
                            digest: request.release.context("package digest is required")?,
                            integration_base: &integration_base,
                            component_base: &component_base,
                            chart: &chart,
                            authority_port,
                        }
                    }
                    Operation::Apply => operation::Application::Apply(
                        request.release.context("package digest is required")?,
                    ),
                    Operation::Diff => operation::Application::Diff(request.release),
                    Operation::Rollback => operation::Application::Rollback(request.revision),
                    Operation::Stop => operation::Application::Stop,
                    Operation::Reset | Operation::Recover | Operation::Close => {
                        unreachable!("handled project operation")
                    }
                },
            )
            .await
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Task {
    ProjectOperation {
        operation: Operation,
        request: Request,
    },
    Environment {
        operation: String,
        project: wamn_registry::ProjectId,
        env: wamn_registry::EnvName,
        document: Option<Value>,
        application: Option<wamn_registry::ApplicationName>,
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

/// Execute an authorized task inside an operation Job.
pub async fn execute_task(
    mut platform: Platform,
    org: &OrgId,
    registry: &str,
    request: Task,
    authority: super::write::AuthorityPort,
) -> anyhow::Result<Value> {
    let mut output = Vec::new();
    match request {
        Task::ProjectOperation { operation, request } => {
            execute(org, registry, platform, operation, request, authority).await
        }
        Task::Environment {
            operation,
            project,
            env,
            document,
            application,
            disposable,
            data,
        } => {
            let triple = wamn_registry::Triple::new(org.clone(), env, project);
            if operation == "environment.delete" {
                let messages = super::delete::delete(&mut platform, &triple, data).await?;
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
            super::lifecycle::configure(&mut platform, document, application.as_ref(), disposable)
                .await?;
            Ok(json!({"configured":true}))
        }
        Task::Schema {
            target,
            projects,
            org: request_org,
            env,
            apply,
        } => {
            ensure!(target == "project", "unknown platform SQL target");
            let env = env.context("environment is absent")?;
            ensure!(
                request_org.as_ref() == Some(org),
                "environment belongs to another org"
            );
            let first = projects.first().context("environment has no projects")?;
            let coordinate = wamn_registry::Triple::new(org.clone(), env.clone(), first.clone());
            super::database::LifecycleLock::hold(
                super::database::DatabaseConnection::WaitForEnvironment {
                    input: &platform.system_database_url,
                    triple: &coordinate,
                },
                &coordinate,
                async |lock| {
                    for project in projects {
                        let triple = Triple::new(org.clone(), env.clone(), project);
                        project_schema(&platform, lock, &triple, apply, &mut output)
                            .await
                            .with_context(|| format!("platform SQL for {}", triple.project))?;
                    }
                    Ok(())
                },
            )
            .await?;
            Ok(json!({"log":String::from_utf8(output)?}))
        }
    }
}

async fn project_schema(
    platform: &Platform,
    lock: &crate::environment_sql_effects::PostgresSession,
    triple: &Triple,
    apply: bool,
    output: &mut Vec<u8>,
) -> anyhow::Result<()> {
    use std::io::Write as _;

    let migration = lock.connection(triple).await?;
    platform.prepare_database(&migration).await?;
    let migration = migration.to_json()?;
    writeln!(output, "Project {}", triple.project)?;
    lock.project_schema(triple, &migration, apply, output).await
}
