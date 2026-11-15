//! Drive Receiving screens through the installed product deployment.
/// The installed platform supplies application and identity services.
#[tokio::test]
#[ignore = "requires: cluster, env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_TIMINGS_ROOT, bin:kubectl, bin:node, bin:pnpm, bin:google-chrome"]
async fn local_receiving_screens() -> anyhow::Result<()> {
    drive().await
}

/// The five sync tests of the open receipt lines screen: a 30 s drop, another
/// browser, a reopen during an outage, a flapping replay, and a refusal.
#[tokio::test]
#[ignore = "requires: cluster, env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_TIMINGS_ROOT, bin:kubectl, bin:node, bin:pnpm, bin:google-chrome"]
async fn local_receiving_sync() -> anyhow::Result<()> {
    drive().await
}

/// The five offline profile tests of the open receipt lines screen: a session
/// reopen after and before the login expires, expired entries, sign-out
/// clearing, and a local_first reopen.
#[tokio::test]
#[ignore = "requires: cluster, env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_TIMINGS_ROOT, bin:kubectl, bin:node, bin:pnpm, bin:google-chrome"]
async fn local_receiving_offline() -> anyhow::Result<()> {
    drive().await
}

async fn drive() -> anyhow::Result<()> {
    panic!("wamn-eulfr.12.9: no product Receiving browser data import");
}
