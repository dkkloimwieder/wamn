//! Fail-closed compatibility shell for the later-removed `pin-run` route.

use anyhow::bail;
use clap::Args;

#[derive(Debug, Args)]
pub struct PinRunArgs {
    /// Retained only until the route is physically removed.
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: String,

    /// Retained only until the route is physically removed.
    #[arg(long, default_value = "wamn_run")]
    pub schema: String,

    /// Retained only until the route is physically removed.
    #[arg(long)]
    pub tenant: String,

    /// Retained only until the route is physically removed.
    #[arg(long)]
    pub run_id: String,

    /// Retained persisted selector field.
    #[arg(long)]
    pub suite_id: String,

    /// Retained only until the route is physically removed.
    #[arg(long)]
    pub case_id: String,

    /// Retained only until the route is physically removed.
    #[arg(long, default_value_t = 0)]
    pub ordinal: i32,

    /// Retained only so old invocations fail with the contract-level refusal.
    #[arg(long = "ignore-path")]
    pub ignore_path: Vec<String>,
}

/// Refuse before opening a database connection or reading captured output.
pub async fn run(_args: PinRunArgs) -> anyhow::Result<()> {
    bail!("pin-run is unavailable under the MVP inline test-set contract")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pin_run_refuses_without_touching_its_database_target() {
        let error = run(PinRunArgs {
            database_url: "not-a-database-url".into(),
            schema: "not_a_schema".into(),
            tenant: "tenant-a".into(),
            run_id: "run-a".into(),
            suite_id: "test-a".into(),
            case_id: "case-a".into(),
            ordinal: 0,
            ignore_path: vec!["/volatile".into()],
        })
        .await
        .expect_err("the retired output-pinning contract must refuse");

        assert_eq!(
            error.to_string(),
            "pin-run is unavailable under the MVP inline test-set contract"
        );
    }
}
