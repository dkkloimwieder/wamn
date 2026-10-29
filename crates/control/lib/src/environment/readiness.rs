//! Step 7 of `apply`: readiness
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! `helm upgrade --wait` returned when the host Deployment was ready. The verb
//! then waits on each role WorkloadDeployment of the release to report
//! `Ready`, with what remains of the readiness budget: the budget bounds the
//! rollout, not each wait in turn. The manifest names no check route, so
//! readiness is the check. A failed wait carries the host pod report.

use std::time::Instant;

use anyhow::{Context as _, bail};

use super::Platform;
use super::ensure::kubectl;
use super::host_report;
use super::write::Written;

/// Wait for every role WorkloadDeployment of the written release.
///
/// # Errors
///
/// When the budget is spent, or a WorkloadDeployment does not report `Ready`
/// in what remains of it. The release still serves; the operator chooses
/// `rollback` or a fix.
pub async fn wait(platform: &Platform, written: &Written) -> anyhow::Result<()> {
    match wait_roles(platform, written).await {
        Ok(()) => Ok(()),
        Err(error) => {
            let report = host_report::collect(&platform.target, &written.release_name).await;
            Err(host_report::attach(&error, &report))
        }
    }
}

async fn wait_roles(platform: &Platform, written: &Written) -> anyhow::Result<()> {
    for role in &written.roles {
        let name = crate::release_chart::workload_name(&written.release_name, *role);
        let remaining = written.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("the readiness budget is spent before WorkloadDeployment {name} is Ready");
        }
        kubectl(
            platform,
            &platform.target.namespace,
            &[
                "wait",
                "--for=condition=Ready",
                &format!("workloaddeployment/{name}"),
                &format!("--timeout={}s", remaining.as_secs().max(1)),
            ],
            None,
        )
        .await
        .with_context(|| format!("wait for WorkloadDeployment {name} to report Ready"))?;
    }
    Ok(())
}
