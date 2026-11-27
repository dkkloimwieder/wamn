//! Stop capture for recovery and retry reader restart after resynchronization.
use super::Platform;
use super::{
    database::{DatabaseConnection, LifecycleLock},
    operation_phase::{Phase, Step},
};
use anyhow::{Context as _, ensure};
pub use recovery::{
    CloseCaptureGapRequest, ClosedCaptureGap, RecoverCaptureGapRequest, RecoveredCaptureGap,
};
use serde_json::json;
use wamn_registry::Triple;

mod plan;
mod recovery;

pub(super) async fn set_paused(
    platform: &Platform,
    triple: &Triple,
    paused: bool,
) -> anyhow::Result<()> {
    let name =
        wamn_registry::name::project_env_resource_name(&triple.org, &triple.project, &triple.env);
    wamn_platform_install::deployment::update_project(
        &platform.cluster,
        &platform.chart,
        &name,
        json!({"cdcReader":{"pause":paused}}),
    )
    .await?;
    if !paused && platform.cluster.reader_exists(triple).await? {
        platform.cluster.wait_reader_ready(triple).await?;
    }
    Ok(())
}

/// Recover a lost capture position while pausing its reader through the project chart.
pub(super) async fn recover(
    platform: &Platform,
    args: &RecoverCaptureGapRequest,
    phase: &Phase<'_>,
) -> anyhow::Result<RecoveredCaptureGap> {
    let triple =
        wamn_registry::Triple::new(args.org.clone(), args.env.clone(), args.project.clone());
    LifecycleLock::hold(
        DatabaseConnection::EnvPhase(&args.system_transaction_url),
        &triple,
        async |lock| {
            let instance = lock.capture_instance(&triple).await?;
            let owner = wamn_postgres::database::connection_for(
                &triple,
                &instance,
                &platform.cluster.namespace,
            );
            let publication_exists = platform
                .cluster
                .publication_exists(&owner.database)
                .await
                .with_context(|| format!("read CDC Publication for {triple}"))?;
            ensure!(
                publication_exists,
                "{triple} has no CDC Publication to recover"
            );
            let capture = wamn_postgres::database::capture_configuration(owner)?;
            platform.prepare_database(&capture).await?;
            recovery::recover_capture_gap(
                args,
                &instance,
                &capture.to_json()?,
                platform,
                lock,
                phase,
            )
            .await
        },
    )
    .await
}

/// Record resynchronization and retry restarting the reader without changing its time.
pub(super) async fn close(
    args: &CloseCaptureGapRequest,
    phase: &Phase<'_>,
) -> anyhow::Result<ClosedCaptureGap> {
    let triple =
        wamn_registry::Triple::new(args.org.clone(), args.env.clone(), args.project.clone());
    LifecycleLock::hold(
        DatabaseConnection::EnvPhase(&args.system_transaction_url),
        &triple,
        async |lock| {
            let closed = lock.close_capture_gap(&triple).await?;
            phase.request(Step::SyncCapture).await?;
            Ok(closed)
        },
    )
    .await
}
