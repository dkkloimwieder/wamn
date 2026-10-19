//! Sign in to the built platform UI through the local control host and identity service.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_control::dev::environment::{DEVELOPER_EMAIL, ENVIRONMENT, ORG, PROJECT};
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
async fn local_sign_in_selects_empty_control_layout() -> anyhow::Result<()> {
    let started = Instant::now();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let mut postgres = wamn_test_postgres::start(&[("wal_level", "logical")])?;
    let system = postgres.create_database("wamn_system")?;
    let root = PathBuf::from(required("WAMN_TIMINGS_ROOT")?).join("platform-ui-env");
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
        .env("WAMN_DEV_ENV_PLATFORM_DOMAIN", "example.invalid")
        .env("RESEND_API_KEY", "unused-development-fixture")
        .env("RESEND_FROM", "WAMN <fixture@example.invalid>");
    let result = async {
        run(&mut up).await?;
        let (mut admin, driver) = tokio_postgres::connect(system.url(), tokio_postgres::NoTls).await?;
        let driver = tokio::spawn(driver);
        let actor: wamn_platform_identity::PrincipalId =
            PlatformComponent::Provisioning.principal_id().to_string().parse()?;
        // Bind the caller for the existing platform verbs. No identity table SQL belongs here.
        admin.execute(
            "SELECT set_config('app.user_id', $1, false), set_config('app.operation', 'admin:platform-ui-test', false)",
            &[&actor.as_str()],
        ).await?;
        // dev up calls provision-org --admin through its shared implementation.
        let user = create_or_reuse_user(&admin, DEVELOPER_EMAIL, "Developer").await?.principal_id;
        grant_project_env_membership(&admin, &user, ORG, PROJECT, ENVIRONMENT).await?;
        let invitation = wamn_platform_identity::password::issue_invitation(&mut admin, &actor, &user).await?;
        let web: Value = serde_json::from_slice(&std::fs::read(root.join("web.json"))?)?;
        let url = web["url"].as_str().context("local platform URL")?;
        let enrolled = reqwest::Client::new().post(format!("{url}/password/enroll"))
            .json(&json!({"principal_id": user.as_str(), "invitation": invitation.secret(), "password": PASSWORD}))
            .send().await?;
        ensure!(enrolled.status() == reqwest::StatusCode::NO_CONTENT, "password enrollment refused: {}", enrolled.status());
        run(Command::new("pnpm").current_dir(&repository)
            .args(["exec", "node", "apps/platform_ui/test/browser.mjs"])
            .env("WAMN_BROWSER_URL", url)
            .env("WAMN_BROWSER_EMAIL", DEVELOPER_EMAIL)
            .env("WAMN_BROWSER_PASSWORD", PASSWORD)
            .env("WAMN_BROWSER_ORG", ORG)
        ).await?;
        drop(admin);
        driver.await??;
        Ok::<_, anyhow::Error>(())
    }.await;
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
        "local_sign_in_selects_empty_control_layout: {:.2}s; result={}; cleanup={}",
        started.elapsed().as_secs_f64(),
        if result.is_ok() { "PASS" } else { "FAIL" },
        if cleanup.is_ok() { "PASS" } else { "FAIL" }
    );
    result?;
    cleanup?;
    ensure!(
        started.elapsed().as_secs() < 900,
        "local sign-in exceeded the 15-minute issue budget"
    );
    Ok(())
}
