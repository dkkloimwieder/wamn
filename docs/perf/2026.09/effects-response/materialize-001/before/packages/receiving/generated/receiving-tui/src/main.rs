// @generated; do not edit.

#[tokio::main]
async fn main() -> Result<wamn_client_terminal::operator::ExitReason, Box<dyn std::error::Error>> {
    wamn_client_terminal::operator::run("wamn_receiving", wamn_generated_receiving_tui::screens).await
}
