#[tokio::main]
async fn main() -> Result<wamn_client_terminal::operator::ExitReason, Box<dyn std::error::Error>> {
    wamn_client_terminal::operator::run("wamn_receiving", wamn_receiving_ui::screens).await
}
