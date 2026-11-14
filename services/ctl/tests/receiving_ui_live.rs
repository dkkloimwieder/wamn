//! Drive Receiving screens through the installed product deployment.
use anyhow::{Context as _, ensure};
use serde_json::json;
use std::process::Stdio;
use std::time::Instant;
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;
#[expect(
    dead_code,
    reason = "The shared fixture also serves control browser cases."
)]
#[path = "support/kind_browser.rs"]
mod kind_browser;
use kind_browser::required;

/// The installed platform supplies application and identity services.
#[tokio::test]
async fn local_receiving_screens() -> anyhow::Result<()> {
    drive(Browser {
        name: "local_receiving_screens",
        script: "apps/wamn_receiving/web/test/browser.mjs",
        environment: "receiving-ui-env",
        screenshots: "receiving-screenshots",
    })
    .await
}

/// The five sync tests of the open receipt lines screen: a 30 s drop, another
/// browser, a reopen during an outage, a flapping replay, and a refusal.
#[tokio::test]
async fn local_receiving_sync() -> anyhow::Result<()> {
    drive(Browser {
        name: "local_receiving_sync",
        script: "apps/wamn_receiving/web/test/sync-browser.mjs",
        environment: "receiving-sync-env",
        screenshots: "receiving-sync-screenshots",
    })
    .await
}

/// The five offline profile tests of the open receipt lines screen: a session
/// reopen after and before the login expires, expired entries, sign-out
/// clearing, and a local_first reopen.
#[tokio::test]
async fn local_receiving_offline() -> anyhow::Result<()> {
    drive(Browser {
        name: "local_receiving_offline",
        script: "apps/wamn_receiving/web/test/offline-browser.mjs",
        environment: "receiving-offline-env",
        screenshots: "receiving-offline-screenshots",
    })
    .await
}

struct Browser {
    name: &'static str,
    script: &'static str,
    environment: &'static str,
    screenshots: &'static str,
}

async fn drive(browser_run: Browser) -> anyhow::Result<()> {
    panic!("wamn-eulfr.12.9: no product Receiving browser data import");
    let _lock = wamn_test_postgres::lock();
    let started = Instant::now();
    let fixture = kind_browser::Fixture::prepare(browser_run.environment).await?;
    let report = fixture.root.join(browser_run.screenshots);
    std::fs::create_dir_all(&report)?;
    println!("Receiving screenshots: {}", report.display());
    let application = fixture
        .environment
        .applications
        .values()
        .next()
        .context("Receiving application")?;
    let mut url = url::Url::parse(&required("WAMN_BROWSER_URL")?)?;
    url.set_host(Some(&application.hostname))?;
    let mut browser = Command::new("pnpm")
        .current_dir(&fixture.repository)
        .args(["exec", "node", browser_run.script])
        .env("WAMN_BROWSER_URL", url.as_str())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = browser.stdin.take().context("browser input")?;
    input
        .write_all(format!("{}\n", json!({"reportDirectory":report})).as_bytes())
        .await?;
    drop(input);
    let output = browser.wait_with_output().await?;
    println!("{}", String::from_utf8_lossy(&output.stdout));
    ensure!(output.status.success(), "browser failed: {}", output.status);
    println!(
        "{}: {:.2}s",
        browser_run.name,
        started.elapsed().as_secs_f64()
    );
    ensure!(
        started.elapsed().as_secs() < 900,
        "sign-in exceeded the 15-minute issue budget"
    );
    Ok(())
}
