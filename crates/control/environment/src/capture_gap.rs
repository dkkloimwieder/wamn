//! Resolve environment identity before calling CDC recovery.
pub use wamn_events::capture_gap::{
    CloseCaptureGapRequest, ClosedCaptureGap, MISSING_SLOT_REASON, RecoverCaptureGapRequest,
    RecoveredCaptureGap, close_capture_gap, close_open_capture_gap,
};

pub async fn recover_capture_gap(
    args: &RecoverCaptureGapRequest,
) -> anyhow::Result<RecoveredCaptureGap> {
    let triple = wamn_registry::Triple::new(&args.org, args.env.as_str(), &args.project);
    let instance =
        crate::provision_project_env::read_project_env_instance(&args.system_database_url, &triple)
            .await?;
    wamn_events::capture_gap::recover_capture_gap(args, &instance, async || Ok(())).await
}
