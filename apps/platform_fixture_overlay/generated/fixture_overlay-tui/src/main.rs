// @generated; do not edit.

#[tokio::main]
async fn main() -> Result<wamn_client_terminal::operator::ExitReason, Box<dyn std::error::Error>> {
    wamn_client_terminal::operator::run(
        "platform_fixture_overlay",
        wamn_generated_fixture_overlay_tui::screens,
    )
    .await
}
