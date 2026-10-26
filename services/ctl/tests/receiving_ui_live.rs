//! Drive Receiving screens through the local application host and identity service.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::Command;
use wamn_control::dev::environment::{
    DEVELOPER_EMAIL, ENVIRONMENT, JourneyScope, ORG, PROJECT, reconcile_journey_run_plane,
};
use wamn_control_provision::PlatformComponent;
use wamn_platform_identity::{create_or_reuse_user, grant_project_env_membership};

const PASSWORD: &str = "local platform browser password";

fn required(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("set {name}; run through delivery_timings"))
}

async fn run(command: &mut Command) -> anyhow::Result<()> {
    let status = command.status().await?;
    ensure!(status.success(), "command failed: {status}");
    Ok(())
}

/// The existing fixture supplies isolated scheduler, events and telemetry services.
#[tokio::test]
#[ignore = "requires: delivery_timings fixture, built native binaries, platform UI, flow-http, materializer, Chrome"]
async fn local_receiving_screens() -> anyhow::Result<()> {
    let started = Instant::now();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let mut postgres = wamn_test_postgres::start(&[("wal_level", "logical")])?;
    let system = postgres.create_database("wamn_system")?;
    let results = PathBuf::from(required("WAMN_TIMINGS_ROOT")?);
    let report = results.join("receiving-screenshots");
    std::fs::create_dir_all(&report)?;
    println!("Receiving screenshots: {}", report.display());
    let root = results.join("receiving-ui-env");
    let wamn = required("WAMN_RECEIVING_DEV_BIN")?;
    let mut up = Command::new(&wamn);
    up.current_dir(&repository)
        .args(["dev", "up", "--root"])
        .arg(&root)
        .arg("--platform-ui")
        .arg(repository.join("apps/platform_ui/dist"))
        .arg("--package")
        .arg(repository.join("apps/wamn_receiving"))
        .env("WAMN_DEV_ENV_SYSTEM_DATABASE_URL", system.url())
        .env(
            "WAMN_DEV_ENV_HOST_BIN",
            required("WAMN_RECEIVING_DEV_HOST_BIN")?,
        )
        .env(
            "WAMN_DEV_ENV_NATS_URL",
            required("WAMN_RECEIVING_DEV_NATS_URL")?,
        )
        .env(
            "WAMN_DEV_ENV_OTEL_EXPORTER_OTLP_ENDPOINT",
            required("WAMN_RECEIVING_DEV_OTEL_EXPORTER_OTLP_ENDPOINT")?,
        )
        .env("WAMN_DEV_ENV_ROUTE_HOST", required("WAMN_ROUTE_HOST")?)
        .env("WAMN_DEV_ENV_PLATFORM_DOMAIN", "example.invalid");
    let mut application = None;
    let result = async {
        run(&mut up).await?;
        std::fs::write(root.join("environment.k"), include_str!("../../../tests/integration/fixtures/local-environments/receiving.k"))?;
        let mut child = Command::new(&wamn)
            .current_dir(&repository)
            .args(["dev", "--config"])
            .arg(root.join("dev.json"))
            .arg("--overlay-root").arg(repository.join("apps/wamn_receiving"))
            .arg("--hold")
            .stdout(Stdio::piped()).stderr(Stdio::inherit())
            .kill_on_drop(true).spawn()?;
        let stdout = child.stdout.take().context("local application output")?;
        application = Some(child);
        let mut lines = BufReader::new(stdout).lines();
        loop {
            let line = lines.next_line().await?.context("local application stopped before readiness")?;
            println!("{line}");
            if line == "run holding" { break; }
        }
        let (mut admin, driver) = tokio_postgres::connect(system.url(), tokio_postgres::NoTls).await?;
        let driver = tokio::spawn(driver);
        let actor: wamn_platform_identity::PrincipalId =
            PlatformComponent::Provisioning.principal_id().to_string().parse()?;
        // Bind the caller for the existing platform verbs. No identity table SQL belongs here.
        admin.execute(
            "SELECT set_config('app.user_id', $1, false), set_config('app.operation', 'admin:receiving-ui-test', false)",
            &[&actor.as_str()],
        ).await?;
        // dev up calls provision-org --admin through its shared implementation.
        let user = create_or_reuse_user(&admin, DEVELOPER_EMAIL, "Developer").await?.principal_id;
        grant_project_env_membership(&admin, &user, ORG, PROJECT, ENVIRONMENT).await?;
        let invitation = wamn_platform_identity::password::issue_invitation(&mut admin, &actor, &user).await?;
        let web: Value = serde_json::from_slice(&std::fs::read(root.join("web.json"))?)?;
        let url = web["applications"].as_array().context("application URLs")?
            .iter().find(|entry| entry["name"] == "wamn_receiving")
            .and_then(|entry| entry["url"].as_str()).context("Receiving URL")?;
        let config: Value = serde_json::from_slice(&std::fs::read(root.join("dev.json"))?)?;
        let target = config["target_database_url"].as_str().context("target database URL")?;
        let tenant = wamn_control_registry::project_env_tenant(ORG, PROJECT, ENVIRONMENT);
        reconcile_journey_run_plane(&JourneyScope { org: ORG.to_owned(), project: PROJECT.to_owned(), environment: ENVIRONMENT.to_owned() }, system.url(), target).await?;
        let (mut business, business_driver) = tokio_postgres::connect(target, tokio_postgres::NoTls).await?;
        let business_driver = tokio::spawn(business_driver);
        wamn_control::user_role::grant_role_on(&mut business, &tenant, DEVELOPER_EMAIL, wamn_project_state::ADMIN_ROLE, &wamn_platform_identity::auth_changes::AuthChanges::default()).await?;
        business.execute(
            "SELECT set_config('app.user_id', $1, false), set_config('app.tenant_id', $2, false), set_config('app.operation', 'admin:receiving-browser-fixture', false)",
            &[&actor.as_str(), &tenant],
        ).await?;
        business.batch_execute(include_str!("fixtures/receiving_ui.sql")).await?;
        drop(business);
        business_driver.await??;
        let enrolled = reqwest::Client::new().post(format!("{url}/password/enroll"))
            .json(&json!({"principal_id": user.as_str(), "invitation": invitation.secret(), "password": PASSWORD}))
            .send().await?;
        ensure!(enrolled.status() == reqwest::StatusCode::NO_CONTENT, "password enrollment refused: {}", enrolled.status());
        let mut browser = Command::new("pnpm").current_dir(&repository)
            .args(["exec", "node", "apps/wamn_receiving/web/test/browser.mjs"])
            .env("WAMN_BROWSER_URL", url)
            .env("WAMN_BROWSER_EMAIL", DEVELOPER_EMAIL)
            .env("WAMN_BROWSER_PASSWORD", PASSWORD)
            .env("WAMN_BROWSER_ORG", ORG)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit())
            .kill_on_drop(true).spawn()?;
        let mut input = browser.stdin.take().context("browser input")?;
        input.write_all(format!("{}\n", json!({"reportDirectory": report})).as_bytes()).await?;
        drop(input);
        let output = browser.wait_with_output().await?;
        println!("{}", String::from_utf8_lossy(&output.stdout));
        ensure!(output.status.success(), "browser failed: {}", output.status);
        ensure!(String::from_utf8_lossy(&output.stdout).contains("PASS: Receiving screens, writes, pagination and navigation"), "Receiving browser assertions did not finish");
        drop(admin);
        driver.await??;
        Ok::<_, anyhow::Error>(())
    }.await;
    let application_cleanup = async {
        if let Some(mut child) = application {
            if let Some(pid) = child.id() {
                run(Command::new("kill").args(["-INT", &pid.to_string()])).await?;
            }
            let status = child.wait().await?;
            ensure!(
                status.success(),
                "local application shutdown failed: {status}"
            );
            Ok::<_, anyhow::Error>(())
        } else {
            Ok(())
        }
    }
    .await;
    let cleanup = if root.join("identity-process.json").exists() {
        run(Command::new(&wamn)
            .current_dir(&repository)
            .args(["dev", "down", "--root"])
            .arg(&root))
        .await
    } else {
        Ok(())
    };
    println!(
        "local_receiving_screens: {:.2}s; result={}; cleanup={}",
        started.elapsed().as_secs_f64(),
        if result.is_ok() { "PASS" } else { "FAIL" },
        if cleanup.is_ok() { "PASS" } else { "FAIL" }
    );
    result?;
    application_cleanup?;
    cleanup?;
    ensure!(
        started.elapsed().as_secs() < 900,
        "local sign-in exceeded the 15-minute issue budget"
    );
    Ok(())
}
