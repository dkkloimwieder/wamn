//! Sign in to the installed platform UI through product control and identity.
use anyhow::{Context as _, ensure};
use serde_json::json;
use std::process::Stdio;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::Command;
#[path = "support/kind_browser.rs"]
mod kind_browser;
const SECOND_EMAIL: &str = "project-admin@example.invalid";
const BROWSER_PROJECT: &str = "browser-project";

#[tokio::test]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_BROWSER_EMAIL, env:WAMN_BROWSER_PASSWORD, env:WAMN_BROWSER_ORG, env:WAMN_TIMINGS_ROOT, bin:node, bin:pnpm, bin:google-chrome"]
async fn local_control_projects_and_membership() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_BROWSER_EMAIL, env:WAMN_BROWSER_PASSWORD, env:WAMN_BROWSER_ORG, env:WAMN_TIMINGS_ROOT, bin:node, bin:pnpm, bin:google-chrome",
    );
    let started = Instant::now();
    let fixture = kind_browser::Fixture::prepare("platform-ui-env").await?;
    let report = fixture.root.join("platform-ui-screenshots");
    std::fs::create_dir_all(&report)?;
    println!("Platform UI screenshots: {}", report.display());
    let release = fixture.status["applications"]
        .as_object()
        .context("application status")?
        .values()
        .next()
        .context("Receiving status")?["current"]
        .clone();
    let mut browser = Command::new("pnpm")
        .current_dir(&fixture.repository)
        .args(["exec", "node", "apps/platform_ui/test/browser.mjs"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = browser.stdin.take().context("browser input")?;
    input.write_all(format!("{}\n", json!({"reportDirectory": report, "email": SECOND_EMAIL, "project": BROWSER_PROJECT, "env": fixture.environment.env, "releaseProject": fixture.environment.project, "releaseEnv": fixture.environment.env, "releaseDigest": release})).as_bytes()).await?;
    let mut output = BufReader::new(browser.stdout.take().context("browser output")?);
    let mut line = String::new();
    output.read_line(&mut line).await?;
    ensure!(
        line.trim() == "invitation-created",
        "browser did not invite the user: {line}"
    );
    let code = fixture.invitation(SECOND_EMAIL).await?;
    input
        .write_all(format!("{}\n", json!({"code": code})).as_bytes())
        .await?;
    line.clear();
    output.read_line(&mut line).await?;
    ensure!(
        line.trim() == "project-created",
        "browser did not create the project: {line}"
    );
    fixture.browser_environment(BROWSER_PROJECT).await?;
    input.write_all(b"environment-ready\n").await?;
    drop(input);
    browser.stdout = Some(output.into_inner());
    let output = browser.wait_with_output().await?;
    println!("{}", String::from_utf8_lossy(&output.stdout));
    ensure!(output.status.success(), "browser failed: {}", output.status);
    ensure!(
            String::from_utf8_lossy(&output.stdout).contains(
                "PASS: audience table replaces sign-in; Control first; org/project/environment rows enter their audiences"
            ),
            "browser did not complete the audience table assertions"
        );
    ensure!(
            String::from_utf8_lossy(&output.stdout).contains(
                "PASS: application role create/delete, permission grant/revoke, user-role grant/revoke and reload state"
            ),
            "browser did not complete application administration"
        );
    ensure!(
            String::from_utf8_lossy(&output.stdout).contains(
                "PASS: environment and Control splash tiles match status values, grouped by project, with one read per environment"
            ),
            "browser did not complete the splash tile assertions"
        );

    ensure!(
        started.elapsed().as_secs() < 900,
        "sign-in exceeded the 15-minute issue budget"
    );
    Ok(())
}
