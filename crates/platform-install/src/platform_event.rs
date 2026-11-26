//! Append command events to the system database journal.

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sqlx::{Connection as _, PgConnection, types::Uuid};
use wamn_digest::Digest;
use wamn_principal::PrincipalId;
use wamn_registry::{AppName, EnvName, OrgId, ProjectId};

/// Lifecycle command recorded in the event journal.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    OrgCreate,
    EnvApply,
    EnvDelete,
    Deploy,
    Rollback,
    SchemaApply,
    SchemaReset,
    CaptureGapRepair,
}

impl EventType {
    fn code(self) -> &'static str {
        match self {
            Self::OrgCreate => "org_create",
            Self::EnvApply => "env_apply",
            Self::EnvDelete => "env_delete",
            Self::Deploy => "deploy",
            Self::Rollback => "rollback",
            Self::SchemaApply => "schema_apply",
            Self::SchemaReset => "schema_reset",
            Self::CaptureGapRepair => "capture_gap_repair",
        }
    }
}

/// Result observed at one phase of a command.
#[derive(Debug, Clone, Copy)]
pub enum Outcome {
    Started,
    Succeeded,
    Failed(&'static str),
    RolledBack,
    Refused,
}

/// Identity and target common to a command's start and result events.
#[derive(Debug, Deserialize, Serialize)]
pub struct Command {
    pub id: Uuid,
    pub event_type: EventType,
    pub principal_id: PrincipalId,
    pub env: Option<EnvName>,
    pub project: Option<ProjectId>,
    pub app: Option<AppName>,
    pub package_digest: Option<Digest>,
    pub helm_revision: Option<i32>,
}

/// Append a command phase once, using the supplied caller and optional Job reference.
///
/// # Errors
///
/// Returns the database error if the caller binding or journal write fails.
pub async fn record(
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
             app, package_digest, helm_revision, job_namespace, job_name, failure_summary)
         SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, left($13, 500)
         WHERE NOT EXISTS (
             SELECT FROM provisioning.platform_event
             WHERE command_id = $1 AND (outcome = 'started') = ($3 = 'started')
         )",
    )
    .bind(command.id)
    .bind(command.event_type.code())
    .bind(outcome)
    .bind(Uuid::from(command.principal_id))
    .bind(org.as_str())
    .bind(command.env.as_ref().map(EnvName::as_str))
    .bind(command.project.as_ref().map(ProjectId::as_str))
    .bind(command.app.as_ref().map(AppName::as_str))
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
