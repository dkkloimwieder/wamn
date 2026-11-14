//! Stop capture for recovery and retry reader restart after resynchronization.
use super::lock::LifecycleLock;
pub use wamn_events::capture_gap::{
    CloseCaptureGapRequest, ClosedCaptureGap, RecoverCaptureGapRequest, RecoveredCaptureGap,
};
use wamn_kubernetes::Target;

/// Recover a lost slot while stopping its Deployment under the environment lock.
pub async fn recover(
    target: &Target,
    args: &RecoverCaptureGapRequest,
) -> anyhow::Result<RecoveredCaptureGap> {
    anyhow::ensure!(
        target.namespace == args.org,
        "reader must run in the org namespace"
    );
    let triple = wamn_registry::Triple::new(&args.org, args.env.as_str(), &args.project);
    LifecycleLock::hold(&args.system_database_url, &triple, async |_| {
        let instance = crate::provision_project_env::read_project_env_instance(
            &args.system_database_url,
            &triple,
        )
        .await?;
        wamn_events::capture_gap::recover_capture_gap(args, &instance, target).await
    })
    .await
}

/// Record resynchronization and retry restarting the reader without changing its time.
pub async fn close(
    target: &Target,
    args: &CloseCaptureGapRequest,
) -> anyhow::Result<ClosedCaptureGap> {
    anyhow::ensure!(
        target.namespace == args.org,
        "reader must run in the org namespace"
    );
    let triple = wamn_registry::Triple::new(&args.org, args.env.as_str(), &args.project);
    LifecycleLock::hold(&args.system_database_url, &triple, async |_| {
        let closed = wamn_events::capture_gap::close_capture_gap(args).await?;
        wamn_events::lifecycle::start_reader(
            &target.kubeconfig,
            &target.context,
            &target.namespace,
            &triple,
        )
        .await?;
        Ok(closed)
    })
    .await
}
