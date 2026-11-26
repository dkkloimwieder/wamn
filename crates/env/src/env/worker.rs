//! Execute a project operation against applied registry configuration.

use super::{
    Platform, capture_gap,
    document::{EnvDocument, EnvFile, WorkloadRelease},
    operation,
};
use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wamn_registry::{OrgId, Triple};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub project: wamn_registry::ProjectId,
    pub env: wamn_registry::EnvName,
    #[serde(default)]
    pub app: Option<wamn_registry::AppName>,
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
            "app.deploy" => Self::Deploy,
            "app.rollback" => Self::Rollback,
            "app.stop" => Self::Stop,
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

/// Execute a project operation using the org and platform artifact origins.
pub async fn execute(
    org: &OrgId,
    org_origin: &str,
    platform_origin: &str,
    platform: Platform,
    (operation, request): (Operation, Request),
    authority_port: super::write::AuthorityPort,
    release: &mut Option<WorkloadRelease>,
) -> anyhow::Result<Value> {
    let triple = Triple::new(org.clone(), request.env, request.project);
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
        Operation::Reset => {
            let document = EnvDocument::read(&platform, &triple).await?;
            operation::reset(&platform, &document).await
        }
        _ => {
            let app = request.app.context("application is required")?;
            let document = EnvDocument::read(&platform, &triple).await?;
            let integration_base;
            let component_base;
            operation::app(
                &platform,
                document,
                &app,
                match operation {
                    Operation::Deploy => {
                        integration_base = format!("{platform_origin}/wamn/integrations");
                        (_, component_base) = wamn_registry::org_artifact_bases(org_origin);
                        operation::App::Deploy {
                            digest: request.release.context("package digest is required")?,
                            integration_base: &integration_base,
                            component_base: &component_base,
                            chart: &chart,
                            authority_port,
                        }
                    }
                    Operation::Apply => operation::App::Apply(
                        request.release.context("package digest is required")?,
                    ),
                    Operation::Diff => operation::App::Diff(request.release),
                    Operation::Rollback => operation::App::Rollback(request.revision),
                    Operation::Stop => operation::App::Stop,
                    Operation::Reset | Operation::Recover | Operation::Close => {
                        unreachable!("handled project operation")
                    }
                },
                release,
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
    Env {
        operation: String,
        project: wamn_registry::ProjectId,
        env: wamn_registry::EnvName,
        document: Option<Value>,
        app: Option<wamn_registry::AppName>,
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

/// Execute an authorized task inside an operation Job and record its resolved release.
pub async fn execute_task(
    mut platform: Platform,
    org: &OrgId,
    org_origin: &str,
    platform_origin: &str,
    request: Task,
    authority: super::write::AuthorityPort,
    release: &mut Option<WorkloadRelease>,
) -> anyhow::Result<Value> {
    let mut output = Vec::new();
    match request {
        Task::ProjectOperation { operation, request } => {
            execute(
                org,
                org_origin,
                platform_origin,
                platform,
                (operation, request),
                authority,
                release,
            )
            .await
        }
        Task::Env {
            operation,
            project,
            env,
            document,
            app,
            disposable,
            data,
        } => {
            let triple = wamn_registry::Triple::new(org.clone(), env, project);
            if operation == "env.delete" {
                let messages = super::delete::delete(&mut platform, &triple, data).await?;
                return Ok(json!({"messages":messages}));
            }
            ensure!(
                operation == "env.configure",
                "unknown environment lifecycle operation"
            );
            let document: EnvFile =
                serde_json::from_value(document.context("environment document is absent")?)?;
            document.require_valid()?;
            let selected = match &app {
                Some(app) => document.app_project(app)?,
                None => &document.projects[0].name,
            };
            ensure!(
                &triple.project == selected
                    && triple.org == document.org
                    && triple.env == document.env,
                "environment document differs from its authorized coordinate"
            );
            super::lifecycle::configure(&mut platform, document, app.as_ref(), disposable).await?;
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
                super::database::DatabaseConnection::WaitForEnv {
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
    lock: &crate::env_sql_effects::PostgresSession,
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
