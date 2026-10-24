//! Sign in to the built platform UI through the local control host and identity service.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::Command;
use wamn_control::dev::environment::{
    DEVELOPER_EMAIL, ENVIRONMENT, JourneyScope, ORG, PROJECT, generation_args,
    install_journey_platform_floor, reconcile_journey_run_plane,
};
use wamn_control::pat_client::PatIssuerConfig;
use wamn_control::provision_project_env::{
    ProvisionProjectEnvRequest, provision_project_env, run_workload_action,
};
use wamn_control_provision::{PlatformComponent, WorkloadRoleFamily};
use wamn_platform_identity::{create_or_reuse_user, grant_project_env_membership};

const PASSWORD: &str = "local platform browser password";
const SECOND_EMAIL: &str = "project-admin@example.invalid";
const BROWSER_PROJECT: &str = "browser-project";

fn required(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("set {name}; run through delivery_timings"))
}

async fn run(command: &mut Command) -> anyhow::Result<()> {
    let status = command.status().await?;
    ensure!(status.success(), "command failed: {status}");
    Ok(())
}

async fn provision_browser_environment(
    admin: &tokio_postgres::Client,
    system_url: &str,
    root: &Path,
) -> anyhow::Result<()> {
    let output = root.join(BROWSER_PROJECT);
    std::fs::create_dir_all(&output)?;
    let provisioned = provision_project_env(&ProvisionProjectEnvRequest {
        org: ORG.to_owned(),
        project: BROWSER_PROJECT.to_owned(),
        env: ENVIRONMENT.to_owned(),
        policy: None,
        disposable: None,
        system_database_url: Some(system_url.to_owned()),
        cluster: Some("route-auth-pg18".to_owned()),
        connection_limit: None,
        cluster_namespace: "wamn-system".to_owned(),
        namespace: "wamn-system".to_owned(),
        secret_namespace: None,
        emit_database: Some(output.join("database.json")),
        emit_role_sql: None,
        emit_privilege_sql: None,
        emit_secret: Some(output.join("database-secret.json")),
        pat_issuer: PatIssuerConfig::default(),
        emit_management_author_pat_secret: None,
        emit_operator_pat_secret: None,
    })
    .await?;
    admin.batch_execute(&provisioned.role_sql).await?;
    // Stand in for the emitted Database CR on the disposable local server.
    admin
        .batch_execute(&format!(
            "{} OWNER wamn_db_owner",
            wamn_control_provision::sql::create_database_named_sql(&provisioned.database)
        ))
        .await?;
    admin.batch_execute(&provisioned.privilege_sql).await?;
    let mut target_url = url::Url::parse(system_url)?;
    target_url.set_path(&provisioned.database);
    let (target, driver) =
        tokio_postgres::connect(target_url.as_str(), tokio_postgres::NoTls).await?;
    let driver = tokio::spawn(driver);
    let scope = JourneyScope {
        org: ORG.to_owned(),
        project: BROWSER_PROJECT.to_owned(),
        environment: ENVIRONMENT.to_owned(),
        tenant: wamn_control_registry::project_env_tenant(ORG, BROWSER_PROJECT, ENVIRONMENT),
    };
    install_journey_platform_floor(&target, "example.invalid").await?;
    reconcile_journey_run_plane(&scope, system_url, target_url.as_str()).await?;
    let secret = output.join("administration.json");
    run_workload_action(&generation_args(
        &scope,
        WorkloadRoleFamily::Administration,
        system_url,
        Some(target_url.as_str()),
        &secret,
    ))
    .await?;
    let patch: Value = serde_json::from_slice(&std::fs::read(
        secret.with_extension("control-administration-patch.json"),
    )?)?;
    for (name, value) in patch["stringData"]
        .as_object()
        .context("administration patch")?
    {
        std::fs::write(
            root.join("control-administration").join(name),
            value.as_str().context("administration login")?,
        )?;
    }
    drop(target);
    driver.await??;
    Ok(())
}

/// The existing fixture supplies isolated scheduler, events and telemetry services.
#[tokio::test]
#[ignore = "requires: delivery_timings fixture, built native binaries, platform UI, flow-http, materializer, Chrome"]
async fn local_control_projects_and_membership() -> anyhow::Result<()> {
    let started = Instant::now();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let mut postgres = wamn_test_postgres::start(&[("wal_level", "logical")])?;
    let system = postgres.create_database("wamn_system")?;
    let results = PathBuf::from(required("WAMN_TIMINGS_ROOT")?);
    let report = results.join("platform-ui-screenshots");
    std::fs::create_dir_all(&report)?;
    println!("Platform UI screenshots: {}", report.display());
    let root = results.join("platform-ui-env");
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
        let endpoint: Value = serde_json::from_slice(&std::fs::read(root.join("application-endpoint.json"))?)?;
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
        let mut browser = Command::new("pnpm").current_dir(&repository)
            .args(["exec", "node", "apps/platform_ui/test/browser.mjs"])
            .env("WAMN_BROWSER_URL", url)
            .env("WAMN_BROWSER_EMAIL", DEVELOPER_EMAIL)
            .env("WAMN_BROWSER_PASSWORD", PASSWORD)
            .env("WAMN_BROWSER_ORG", ORG)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit())
            .kill_on_drop(true).spawn()?;
        let mut input = browser.stdin.take().context("browser input")?;
        input.write_all(format!("{}\n", json!({"reportDirectory": report, "email": SECOND_EMAIL, "project": BROWSER_PROJECT, "env": ENVIRONMENT, "releaseProject": PROJECT, "releaseEnv": ENVIRONMENT, "releaseDigest": endpoint["digest"]})).as_bytes()).await?;
        let mut output = BufReader::new(browser.stdout.take().context("browser output")?);
        let mut line = String::new();
        output.read_line(&mut line).await?;
        ensure!(line.trim() == "invitation-created", "browser did not invite the user: {line}");
        let identity: Value = serde_json::from_slice(&std::fs::read(root.join("identity-process.json"))?)?;
        let mail = std::fs::read_to_string(Path::new(identity["directory"].as_str().context("identity directory")?).join("mail.jsonl"))?;
        let invitation = mail.lines().map(serde_json::from_str::<Value>).collect::<Result<Vec<_>, _>>()?
            .into_iter().find(|message| message["to"] == SECOND_EMAIL && message["subject"] == "Your WAMN invitation")
            .context("local sender did not record the browser invitation")?;
        let code = invitation["text"].as_str().context("invitation text")?.lines()
            .find_map(|line| line.strip_prefix("Invitation code: ")).context("invitation code")?;
        input.write_all(format!("{}\n", json!({"code": code})).as_bytes()).await?;
        line.clear();
        output.read_line(&mut line).await?;
        ensure!(line.trim() == "project-created", "browser did not create the project: {line}");
        provision_browser_environment(&admin, system.url(), &root).await?;
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
        "local_control_projects_and_membership: {:.2}s; result={}; cleanup={}",
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
