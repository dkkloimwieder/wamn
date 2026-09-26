// @generated; do not edit.

#[tokio::main]
async fn main() -> Result<wamn_client_terminal::operator::ExitReason, Box<dyn std::error::Error>> {
    wamn_client_terminal::operator::run("edge_samples", wamn_generated_samples_tui::screens).await
}
