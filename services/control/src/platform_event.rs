//! The system database journal for authenticated lifecycle commands.

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sqlx::{Connection as _, PgConnection};
use wamn_digest::Digest;
use wamn_identity_protocol::PrincipalId;
use wamn_registry::{ApplicationName, EnvName, OrgId, ProjectId};

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum EventType {
    EnvironmentApply,
    EnvironmentDelete,
    Deploy,
    Rollback,
    SchemaApply,
    SchemaReset,
    CaptureGapRepair,
}

impl EventType {
    fn for_route(operation: &str) -> Option<Self> {
        Some(match operation {
            "environment.configure" => Self::EnvironmentApply,
            "environment.delete" => Self::EnvironmentDelete,
            "application.deploy" => Self::Deploy,
            "application.rollback" => Self::Rollback,
            "schema.apply" | "platform.schema" => Self::SchemaApply,
            "schema.reset" => Self::SchemaReset,
            "capture_gap.recover" | "capture_gap.close" => Self::CaptureGapRepair,
            _ => return None,
        })
    }

    fn code(self) -> &'static str {
        match self {
            Self::EnvironmentApply => "environment_apply",
            Self::EnvironmentDelete => "environment_delete",
            Self::Deploy => "deploy",
            Self::Rollback => "rollback",
            Self::SchemaApply => "schema_apply",
            Self::SchemaReset => "schema_reset",
            Self::CaptureGapRepair => "capture_gap_repair",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Outcome {
    Started,
    Succeeded,
    Failed(&'static str),
    RolledBack,
    Refused,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct Command {
    pub(crate) id: uuid::Uuid,
    event_type: EventType,
    principal_id: PrincipalId,
    env: Option<EnvName>,
    project: Option<ProjectId>,
    application: Option<ApplicationName>,
    pub(crate) package_digest: Option<Digest>,
    pub(crate) helm_revision: Option<i32>,
}

pub(crate) fn command(
    operation: &str,
    principal_id: PrincipalId,
    payload: &str,
) -> Option<Command> {
    let event_type = EventType::for_route(operation)?;
    let input: serde_json::Value = serde_json::from_str(payload).unwrap_or_default();
    if operation == "platform.schema" && input["apply"] != true {
        return None;
    }
    let project = input["project"].as_str().or_else(|| {
        // Platform schema apply can operate on an entire environment.
        let [project] = input["projects"].as_array()?.as_slice() else {
            return None;
        };
        project.as_str()
    });
    Some(Command {
        id: uuid::Uuid::new_v4(),
        event_type,
        principal_id,
        env: input["env"].as_str().and_then(|env| env.parse().ok()),
        project: project.and_then(|project| project.parse().ok()),
        application: input["application"]
            .as_str()
            .and_then(|app| app.parse().ok()),
        package_digest: input["release"]
            .as_str()
            .and_then(|digest| digest.parse().ok()),
        helm_revision: input["revision"]
            .as_u64()
            .and_then(|rev| i32::try_from(rev).ok()),
    })
}

pub(crate) async fn record(
    connection: &mut PgConnection,
    org: &OrgId,
    command: &Command,
    outcome: Outcome,
    job: Option<(&str, &str)>,
) -> anyhow::Result<()> {
    let (outcome, summary) = match outcome {
        Outcome::Started => ("started", None),
        Outcome::Succeeded => ("succeeded", None),
        Outcome::Failed(summary) => ("failed", Some(summary)),
        Outcome::RolledBack => ("rolled_back", None),
        Outcome::Refused => ("refused", None),
    };
    let mut transaction = connection.begin().await?;
    wamn_postgres::bind_transaction_identity(
        &mut transaction,
        &command.principal_id,
        "",
        "",
        None,
        None,
    )
    .await
    .context("bind the platform event principal")?;
    // A timed-out Job submission and the worker can both observe a terminal result.
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('platform_event:' || $1::uuid::text, 0))",
    )
    .bind(command.id)
    .execute(&mut *transaction)
    .await
    .context("serialize journal writes for the command")?;
    sqlx::query(
        "INSERT INTO provisioning.platform_event
            (command_id, event_type, outcome, principal_id, org, env, project,
             application, package_digest, helm_revision, job_namespace, job_name, failure_summary)
         SELECT $1, $2, $3, $4, $5,
                (SELECT env FROM registry.environments WHERE org = $5 AND env = $6),
                (SELECT id FROM registry.projects WHERE org = $5 AND id = $7),
                $8, $9, $10, $11, $12, left($13, 500)
         WHERE NOT EXISTS (
             SELECT FROM provisioning.platform_event
             WHERE command_id = $1 AND (outcome = 'started') = ($3 = 'started')
         )",
    )
    .bind(command.id)
    .bind(command.event_type.code())
    .bind(outcome)
    .bind(uuid::Uuid::from(command.principal_id))
    .bind(org.as_str())
    .bind(command.env.as_ref().map(EnvName::as_str))
    .bind(command.project.as_ref().map(ProjectId::as_str))
    .bind(command.application.as_ref().map(ApplicationName::as_str))
    .bind(command.package_digest.map(|digest| digest.to_string()))
    .bind(command.helm_revision)
    .bind(job.map(|(namespace, _)| namespace))
    .bind(job.map(|(_, name)| name))
    .bind(summary)
    .execute(&mut *transaction)
    .await
    .with_context(|| format!("record {} {outcome}", command.event_type.code()))?;
    transaction
        .commit()
        .await
        .context("commit the platform event")
}

#[cfg(test)]
mod tests;
