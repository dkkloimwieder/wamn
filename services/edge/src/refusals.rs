//! `wamn-edge samples`: list and resolve refused samples while the edge is
//! stopped.
//!
//! The platform refused these samples, and the forward never sends them again.
//! An operator reads the platform's reason, acts on the platform or the
//! device, and resolves each sample with a basis, as an uncertain intent is
//! resolved. A resolved sample is never forwarded.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context as _, bail};
use wamn_run_state::operator_action::OperatorActionBasis;
use wamn_run_state_sqlite::SqliteIntentStore;

use crate::samples::SampleStore;

/// How to call the samples commands.
pub const USAGE: &str = "usage: wamn-edge [--config <path>] samples list, or wamn-edge \
     [--config <path>] samples resolve <sample_key> \
     <external-evidence|counterparty-confirmation|operator-judgment>";

/// Run one samples command over the run-state file at `db`, and return its
/// output.
///
/// # Errors
///
/// Fails when the arguments do not match [`USAGE`], when the edge holds the
/// file, or when the store refuses the command.
pub async fn run(args: &[String], db: &Path) -> anyhow::Result<String> {
    let store = SqliteIntentStore::open(db).with_context(|| {
        format!(
            "open {}; stop the edge first, because a running edge holds the file",
            db.display()
        )
    })?;
    let samples = SampleStore::open(store).await?;
    match args {
        [command] if command == "list" => {
            let mut output = String::new();
            for sample in samples.refused_samples().await? {
                writeln!(
                    output,
                    "{}\t{}\t{}\t{}",
                    sample.sample_key, sample.captured_at, sample.attempts, sample.reason
                )?;
            }
            Ok(output)
        }
        [command, sample_key, basis] if command == "resolve" => {
            let basis: OperatorActionBasis = basis.parse()?;
            samples.resolve(sample_key, basis).await?;
            Ok(format!("resolved sample {sample_key} by {basis}\n"))
        }
        _ => bail!(USAGE),
    }
}
