//! Keep the deployed reader stopped until operator resynchronization finishes.
use super::{Platform, document::EnvironmentDocument, ensure, lock::LifecycleLock, runtime};
use crate::capture_gap::{
    CloseCaptureGapRequest, ClosedCaptureGap, RecoverCaptureGapRequest, RecoveredCaptureGap,
};

pub async fn recover(
    platform: &Platform,
    args: &RecoverCaptureGapRequest,
) -> anyhow::Result<RecoveredCaptureGap> {
    let triple = wamn_registry::Triple::new(&args.org, args.env.as_str(), &args.project);
    let mut platform = platform.clone();
    let document = EnvironmentDocument::read_serving(&platform, &triple).await?;
    let private = ensure::PrivateDirectory::create()?;
    runtime::resolve_target(&mut platform, &document, &private.path(".")).await?;
    LifecycleLock::hold(&platform.system_database_url, &triple, async |_| {
        let instance = crate::provision_project_env::read_project_env_instance(
            &platform.system_database_url,
            &triple,
        )
        .await?;
        wamn_events::capture_gap::recover_capture_gap(args, &instance, async || {
            wamn_events::lifecycle::stop_reader(
                &platform.target.kubeconfig,
                &platform.target.context,
                &platform.target.namespace,
                &triple,
            )
            .await
        })
        .await
    })
    .await
}

pub async fn close(
    platform: &Platform,
    args: &CloseCaptureGapRequest,
) -> anyhow::Result<ClosedCaptureGap> {
    let triple = wamn_registry::Triple::new(&args.org, args.env.as_str(), &args.project);
    let mut platform = platform.clone();
    let document = EnvironmentDocument::read_serving(&platform, &triple).await?;
    let private = ensure::PrivateDirectory::create()?;
    runtime::resolve_target(&mut platform, &document, &private.path(".")).await?;
    LifecycleLock::hold(&platform.system_database_url, &triple, async |_| {
        let closed = crate::capture_gap::close_capture_gap(args).await?;
        let name = wamn_control_provision::project_env_cdc_secret_name(
            &triple.org,
            &triple.project,
            &triple.env,
        );
        ensure::kubectl(
            &platform,
            &platform.target.namespace,
            &["scale", "deployment", &name, "--replicas=1"],
            None,
        )
        .await?;
        ensure::kubectl(
            &platform,
            &platform.target.namespace,
            &[
                "rollout",
                "status",
                &format!("deployment/{name}"),
                "--timeout=120s",
            ],
            None,
        )
        .await?;
        Ok(closed)
    })
    .await
}
