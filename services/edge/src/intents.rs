//! `wamn-edge intents`: list and resolve uncertain intents while the edge is stopped.
//!
//! The running edge holds the SQLite file, so these commands open it only when
//! the edge is stopped. An uncertain intent began and never finished, so its
//! item answers `intent-uncertain` until an operator resolves it here. After
//! that, its key answers `intent-resolved` with the basis, and the caller sends
//! a new key.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context as _, bail};
use wamn_run_state::IntentStore as _;
use wamn_run_state::intent_store::IntentId;
use wamn_run_state::operator_action::OperatorActionBasis;
use wamn_run_state_sqlite::SqliteIntentStore;

/// How to call the intents commands.
pub const USAGE: &str = "usage: wamn-edge [--config <path>], or wamn-edge [--config <path>] \
     intents list, or wamn-edge [--config <path>] intents resolve <id> \
     <external-evidence|counterparty-confirmation|operator-judgment>";

/// Run one intents command over the run-state file at `db`, and return its output.
///
/// # Errors
///
/// Fails when the arguments do not match [`USAGE`], when the edge holds the
/// file, or when the store refuses the command.
pub async fn run(args: &[String], db: &Path) -> anyhow::Result<String> {
    let open = || {
        SqliteIntentStore::open(db).with_context(|| {
            format!(
                "open {}; stop the edge first, because a running edge holds the file",
                db.display()
            )
        })
    };
    match args {
        [command] if command == "list" => {
            let mut output = String::new();
            for intent in open()?.uncertain(u32::MAX).await? {
                writeln!(
                    output,
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    intent.id.0,
                    intent.operation,
                    intent.idempotency_key,
                    intent.package,
                    intent.release,
                    intent.tenant
                )?;
            }
            Ok(output)
        }
        [command, id, basis] if command == "resolve" => {
            let basis: OperatorActionBasis = basis.parse()?;
            open()?.resolve(&IntentId(id.clone()), basis).await?;
            Ok(format!("resolved intent {id} by {basis}\n"))
        }
        _ => bail!(USAGE),
    }
}
