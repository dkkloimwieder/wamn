// @generated; do not edit.

#[tokio::main]
async fn main() -> Result<wamn_client_terminal::operator::ExitReason, Box<dyn std::error::Error>> {
    wamn_client_terminal::operator::run("edge_device", wamn_generated_device_tui::screens).await
}
