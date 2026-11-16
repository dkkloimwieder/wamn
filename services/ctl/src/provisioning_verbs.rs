//! authenticated capture-gap clients.

use clap::Args;

#[derive(Debug, Args)]
pub struct RecoverCaptureGapArgs {
    #[arg(long)]
    pub project: wamn_registry::ProjectId,
    #[arg(long)]
    pub env: wamn_registry::EnvName,
}

#[derive(Debug, Args)]
pub struct CloseCaptureGapArgs {
    #[arg(long)]
    pub project: wamn_registry::ProjectId,
    #[arg(long)]
    pub env: wamn_registry::EnvName,
}

/// Recover one capture gap and print the row it wrote.
pub async fn recover_capture_gap(args: RecoverCaptureGapArgs) -> anyhow::Result<()> {
    let value = crate::operation_verbs::ControlHost::from_env()?
        .call(
            "capture_gap.recover",
            &serde_json::json!({"project":args.project,"env":args.env}),
        )
        .await?;
    println!("{value}");
    Ok(())
}

/// Close the newest recorded gap through authenticated control.
pub async fn close_capture_gap(args: CloseCaptureGapArgs) -> anyhow::Result<()> {
    let value = crate::operation_verbs::ControlHost::from_env()?
        .call(
            "capture_gap.close",
            &serde_json::json!({"project":args.project,"env":args.env}),
        )
        .await?;
    println!("{value}");
    Ok(())
}
