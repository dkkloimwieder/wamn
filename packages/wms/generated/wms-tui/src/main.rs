// @generated; do not edit.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    wamn_client_terminal::operator::run("wamn_wms", wamn_generated_wms_tui::screens).await
}
