//! Repair lost capture positions and record operator resynchronization.
//! Recovery keeps the reader stopped. Control restarts it after closing the gap.

use super::plan::pg_lsn;
use crate::env::database::{MISSING_CAPTURE_REASON, PendingCaptureGap};
use crate::env_sql_effects::{self, PostgresSession};

use anyhow::ensure;
use chrono::{DateTime, Utc};

use wamn_platform_install::env::validate_project_env_cdc;
use wamn_registry::{
    Triple,
    name::{cdc_object_name, project_env_database_name},
};

/// Inputs of one capture gap recovery.
#[derive(Debug)]
pub struct RecoverCaptureGapRequest {
    pub org: wamn_registry::OrgId,
    pub project: wamn_registry::ProjectId,
    pub env: wamn_registry::EnvName,

    /// Apply connection of the system database: the instance suffix, the reader
    /// registration, and the gap row.
    pub system_transaction_url: String,

    /// The NATS of the event streams, with the client certificate.
    pub nats: wamn_event::deploy_broker::EventObserver,
}

/// Inputs of one capture gap close.
#[derive(Debug)]
pub struct CloseCaptureGapRequest {
    pub org: wamn_registry::OrgId,
    pub project: wamn_registry::ProjectId,
    pub env: wamn_registry::EnvName,

    /// Apply connection of the system database.
    pub system_transaction_url: String,
}

/// The gap row `recover-capture-gap` wrote.
#[derive(Debug)]
pub struct RecoveredCaptureGap {
    pub triple: Triple,
    pub replication_name: String,
    pub reason: String,
    pub start_lsn: Option<String>,
    pub start_at: DateTime<Utc>,
    pub end_lsn: String,
    pub created_at: DateTime<Utc>,
}

/// The gap row `close-capture-gap` closed.
#[derive(Debug)]
pub struct ClosedCaptureGap {
    pub triple: Triple,
    pub replication_name: String,
    pub created_at: DateTime<Utc>,
    pub resync_at: DateTime<Utc>,
}

/// Recreate the lost capture position under the same name, and record the gap.
pub(super) async fn recover_capture_gap(
    args: &RecoverCaptureGapRequest,
    instance: &wamn_registry::instance_suffix::InstanceSuffix,
    admin_database_url: &str,
    platform: &super::super::Platform,
    system: &PostgresSession,
    phase: &crate::env::operation_phase::Phase<'_>,
) -> anyhow::Result<RecoveredCaptureGap> {
    validate_project_env_cdc(&args.org, &args.project, &args.env)
        .map_err(|e| anyhow::anyhow!("cdc names: {e}"))?;
    let triple = Triple::new(args.org.clone(), args.env.clone(), args.project.clone());
    let replication_name = cdc_object_name(&args.org, &args.project, &args.env, instance);
    let database = project_env_database_name(&args.org, &args.project, &args.env, instance);

    let registration = system.capture_registration(&triple).await?;
    ensure!(
        registration.name == replication_name,
        "the registration of {triple} names slot {}, not {replication_name}",
        registration.name
    );
    let (status, mut admin) =
        env_sql_effects::inspect_capture(admin_database_url, &database, &replication_name, &triple)
            .await?;
    let reason = status.reason;
    let last = platform
        .broker
        .last_capture_event(&args.nats, &registration.stream, &triple)
        .await?;
    let start_lsn = if reason == MISSING_CAPTURE_REASON {
        last.map(|(lsn, _)| pg_lsn(lsn))
    } else {
        status.confirmed
    };
    let start_at = last.map_or(registration.created_at, |(_, at)| at);

    // Refusals leave a healthy Deployment running. Stop it before the repaired
    // capture position can become visible to a restarting reader.
    phase
        .request(crate::env::operation_phase::Step::PauseCapture)
        .await?;

    let end_lsn = env_sql_effects::repair_capture(&mut admin, &replication_name).await?;
    let created_at = system
        .record_capture_gap(&PendingCaptureGap {
            triple: &triple,
            name: &replication_name,
            reason: &reason,
            start_lsn: start_lsn.as_deref(),
            start_at,
            end_lsn: &end_lsn,
        })
        .await?;

    Ok(RecoveredCaptureGap {
        triple,
        replication_name,
        reason,
        start_lsn,
        start_at,
        end_lsn,
        created_at,
    })
}
