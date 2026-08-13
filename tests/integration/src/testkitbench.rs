//! Compatibility router for the temporarily retained stored-test executor.
//!
//! The former file-case assertion harness used the removed
//! output, port, database, and egress assertion families. The remaining path
//! delegates only the persisted selector protocol, whose physical deletion is
//! owned by the stored-test closing work.

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::Args;

#[derive(Debug, Args)]
pub struct TestKitBenchArgs {
    /// The flowrunner guest used by the retained stored-test worker.
    #[arg(long, default_value = "/bench/flowrunner.wasm")]
    pub flowrunner: PathBuf,

    /// Product stored-test worker executable used by the compatibility adapter.
    #[arg(long, default_value = "/usr/local/bin/wamn-scenario-worker")]
    pub scenario_worker: PathBuf,

    /// Application-role PostgreSQL URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL used only by the disposable compatibility proof.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Stored selector `<flow_id>@<version>` (repeatable).
    #[arg(long = "suite")]
    pub suites: Vec<String>,

    /// Tenant for direct stored selection.
    #[arg(long)]
    pub tenant: Option<String>,

    /// JSON array of persisted selector tuples.
    #[arg(long)]
    pub impact_report: Option<PathBuf>,

    /// Schema containing the temporarily retained persisted test tables.
    #[arg(long, default_value = "wamn_run")]
    pub source_schema: String,

    /// Base name for disposable execution schemas.
    #[arg(long, default_value = "tk_suiteexec")]
    pub exec_schema: String,

    /// Publish and execute the hermetic compatibility fixture.
    #[arg(long)]
    pub seed_demo: bool,

    /// Retain the disposable source schema after a demo run.
    #[arg(long)]
    pub keep: bool,
}

pub async fn run(args: TestKitBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    if args.impact_report.is_some() && !args.suites.is_empty() {
        bail!("choose either --suite or --impact-report, not both");
    }
    if args.seed_demo && (args.impact_report.is_some() || !args.suites.is_empty()) {
        bail!("--seed-demo is standalone; do not combine it with --suite or --impact-report");
    }
    if !args.seed_demo && args.suites.is_empty() && args.impact_report.is_none() {
        bail!("stored-test execution requires --seed-demo, --suite, or --impact-report");
    }

    wamn_test_infrastructure::scenario_worker_gate::run(
        wamn_test_infrastructure::scenario_worker_gate::StoredSuiteGateArgs {
            worker: args.scenario_worker,
            flowrunner: args.flowrunner,
            database_url: args.database_url,
            admin_database_url: args
                .admin_database_url
                .context("stored-test execution needs an administrative database URL")?,
            suites: args.suites,
            tenant: args.tenant,
            impact_report: args.impact_report,
            source_schema: args.source_schema,
            execution_schema_base: args.exec_schema,
            seed_demo: args.seed_demo,
            keep: args.keep,
        },
    )
    .await
}
