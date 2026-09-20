//! The Acme Receiving operator launcher.

use std::error::Error;

use wamn_client_terminal::operator::ExitReason;
use wamn_receiving_tui::{ReceivingApplication, runtime};

#[tokio::main]
async fn main() -> Result<ExitReason, Box<dyn Error>> {
    runtime::run("Acme Receiving", ReceivingApplication::acme).await
}
