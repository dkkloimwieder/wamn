//! Stop capture for recovery and retry reader restart after resynchronization.
use super::Platform;
pub use recovery::{
    CloseCaptureGapRequest, ClosedCaptureGap, RecoverCaptureGapRequest, RecoveredCaptureGap,
};
use serde_json::json;
use wamn_catalog::lifecycle_lock::LifecycleLock;
use wamn_registry::Triple;

mod plan;
mod recovery;

/// Whether the project has any capture gap that still needs resynchronization.
pub(super) async fn is_open(
    client: &mut sqlx::PgConnection,
    triple: &Triple,
) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT FROM registry.capture_gap
         WHERE org = $1 AND project = $2 AND env = $3 AND resync_at IS NULL)",
    )
    .bind(&triple.org)
    .bind(&triple.project)
    .bind(&triple.env)
    .fetch_one(client)
    .await?)
}

pub(super) async fn set_paused(
    platform: &Platform,
    triple: &Triple,
    paused: bool,
) -> anyhow::Result<()> {
    let name =
        wamn_registry::name::environment_resource_name(&triple.org, &triple.project, &triple.env);
    wamn_platform_install::environment::update_project(
        &platform.target,
        &platform.chart,
        &name,
        &json!({"cdcReader":{"pause":paused}}),
    )
    .await
}

/// Recover a lost slot while pausing its reader through the project chart.
pub async fn recover(
    platform: &Platform,
    args: &RecoverCaptureGapRequest,
) -> anyhow::Result<RecoveredCaptureGap> {
    let triple =
        wamn_registry::Triple::new(args.org.clone(), args.env.clone(), args.project.clone());
    LifecycleLock::hold(
        wamn_postgres::connect(&args.system_database_url).await?,
        &triple,
        async |_| {
            let instance = crate::provision_project_env::read_project_env_instance(
                &args.system_database_url,
                &triple,
            )
            .await?;
            recovery::recover_capture_gap(args, &instance, platform).await
        },
    )
    .await
}

/// Record resynchronization and retry restarting the reader without changing its time.
pub async fn close(
    platform: &Platform,
    args: &CloseCaptureGapRequest,
) -> anyhow::Result<ClosedCaptureGap> {
    let triple =
        wamn_registry::Triple::new(args.org.clone(), args.env.clone(), args.project.clone());
    LifecycleLock::hold(
        wamn_postgres::connect(&args.system_database_url).await?,
        &triple,
        async |lock| {
            let closed = recovery::close_capture_gap(args).await?;
            let paused = is_open(&mut *lock.client().await, &triple).await?;
            set_paused(platform, &triple, paused).await?;
            Ok(closed)
        },
    )
    .await
}
