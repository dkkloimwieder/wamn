//! Stop capture for recovery and retry reader restart after resynchronization.
use super::Platform;
use anyhow::{Context as _, ensure};
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
        json!({"cdcReader":{"pause":paused}}),
    )
    .await?;
    if !paused {
        let target = &platform.target;
        let reader = wamn_provision::cdc_reader_name(&triple.org, &triple.project, &triple.env);
        let reader_exists = wamn_kubernetes::kubectl::get::<serde_json::Value>(
            &target.kubeconfig,
            &target.context,
            &target.namespace,
            "deployment",
            &reader,
        )
        .await
        .with_context(|| format!("read CDC reader Deployment {reader}"))?
        .is_some();
        if reader_exists {
            super::cdc_reader::wait_reader_ready(target, triple).await?;
        }
    }
    Ok(())
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
        async |lock| {
            let instance = crate::provision_project_env::read_project_env_instance(
                &mut *lock.client().await,
                &triple,
            )
            .await?;
            let owner = wamn_platform_install::environment::connection_for(&triple, &instance);
            let target = &platform.target;
            let publication_exists = wamn_kubernetes::kubectl::get::<serde_json::Value>(
                &target.kubeconfig,
                &target.context,
                &target.namespace,
                "publication.postgresql.cnpg.io",
                &owner.database,
            )
            .await
            .with_context(|| format!("read CDC Publication for {triple}"))?
            .is_some();
            ensure!(
                publication_exists,
                "{triple} has no CDC Publication to recover"
            );
            let capture = wamn_platform_install::environment::capture_configuration(owner)?;
            platform.prepare_database(&capture).await?;
            recovery::recover_capture_gap(args, &instance, &capture.to_json()?, platform, lock)
                .await
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
            let closed = recovery::close_capture_gap(&triple, &mut *lock.client().await).await?;
            let paused = is_open(&mut *lock.client().await, &triple).await?;
            set_paused(platform, &triple, paused).await?;
            Ok(closed)
        },
    )
    .await
}
