//! Remove explicitly named disposable test resources after an interrupted case.

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let name = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("a disposable cluster name is required"))?;
    let result = wamn_test_infrastructure::cluster::remove_owned(&name).await?;
    if !result.leftovers.is_empty() {
        eprintln!("leftover kind resources: {:?}", result.leftovers);
    }
    result.result()?;
    if !result.leftovers.is_empty() {
        std::process::exit(2);
    }
    Ok(())
}
