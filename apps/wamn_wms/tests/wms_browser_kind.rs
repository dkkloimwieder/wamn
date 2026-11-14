//! Packaging browser contracts through product deployment and create routes.

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use sqlx::Row as _;
use std::path::Path;
use std::process::Stdio;
use wamn_test_infrastructure::scratch::ScratchRoot;
mod release_bindings;
use wamn_test_application::Application;

const LOCATION_A_ID: &str = "00000000-0000-0000-0000-000000000201";
const LOCATION_B_ID: &str = "00000000-0000-0000-0000-000000000202";
const PACKAGING_ID: &str = "00000000-0000-0000-0000-000000000301";
const PREREQUISITES: &str = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL, env:WAMN_SYNC_BENCH_DIST, bin:node, bin:google-chrome";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL, env:WAMN_SYNC_BENCH_DIST, bin:node, bin:google-chrome"]
async fn offline_creates_survive_reload_and_follow_the_feed() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(PREREQUISITES);
    run("offline").await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL, env:WAMN_SYNC_BENCH_DIST, bin:node, bin:google-chrome"]
async fn a_refused_create_succeeds_after_correction() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(PREREQUISITES);
    run("refusal").await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL, env:WAMN_SYNC_BENCH_DIST, bin:node, bin:google-chrome"]
async fn a_lost_create_reply_replays_without_another_row() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(PREREQUISITES);
    run("lost-reply").await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL, env:WAMN_SYNC_BENCH_DIST, bin:node, bin:google-chrome"]
async fn a_waiting_shell_reloads_after_pending_and_sending_entries_finish() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(PREREQUISITES);
    run("release").await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL, env:WAMN_SYNC_BENCH_DIST, bin:node, bin:google-chrome"]
async fn an_old_binding_entry_gets_an_ordinary_refusal() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(PREREQUISITES);
    run("old-entry").await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL, env:WAMN_SYNC_BENCH_DIST, bin:node, bin:google-chrome"]
async fn take_offline_fills_packaging_and_locations() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(PREREQUISITES);
    run("take-offline").await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL, env:WAMN_SYNC_BENCH_DIST, bin:node, bin:google-chrome"]
async fn case2_offline_reopens_upload_three_rows_across_a_release() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(PREREQUISITES);
    run("case2").await
}

async fn run(scenario: &str) -> anyhow::Result<()> {
    let scratch = ScratchRoot::create()?;
    let app = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("WMS source")?
        .to_owned();
    let application = Application::prepare(&app).await?;
    let mut admin = application.observer().await?;
    let audience = application.audience().await?;
    let result = async {
        for (id, code) in [(LOCATION_A_ID, "LOC-A"), (LOCATION_B_ID, "LOC-B")] {
            create(&application, "/location/create", json!({"request_id":id,"id":id,"idempotency_key":id,"location_code":code})).await?;
        }
        create(&application, "/packaging/create", json!({"request_id":PACKAGING_ID,"id":PACKAGING_ID,"idempotency_key":PACKAGING_ID,
            "packaging_code":"PAL-301","type":"pallet","location_id":LOCATION_A_ID,"status":"available"})).await?;
        if scenario == "take-offline" {
            locations(&application).await?;
        }
        let bench = app.join("../../web/sync-bench").canonicalize()?;
        if scenario == "old-entry" {
            release_bindings::bindings(&app, scratch.path())?;
        }
        let driver = match scenario {
            "release" | "old-entry" => "bench/release.mjs",
            "take-offline" => "bench/take-offline.mjs",
            "case2" => "bench/case2.mjs",
            _ => "bench/outbox.mjs",
        };
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(if matches!(scenario, "take-offline" | "case2") {
                900
            } else {
                180
            }),
            tokio::process::Command::new("node")
                .arg(driver)
                .arg(scenario)
                .current_dir(&bench)
                .env(
                    "WAMN_SYNC_BENCH_DIST",
                    std::env::var_os("WAMN_SYNC_BENCH_DIST").context("WAMN_SYNC_BENCH_DIST")?,
                )
                .env("WAMN_ENDPOINT", &application.routes.endpoint)
                .env("WAMN_ROUTE_HOST", &application.routes.host)
                .env("WAMN_BEARER", &application.routes.bearer)
                .env("WAMN_USER_ID", application.caller.as_str())
                .env("WAMN_AUD", audience)
                .env("WAMN_LOCATION_ID", LOCATION_A_ID)
                .env("WAMN_FIRST_ID", PACKAGING_ID)
                .env("WAMN_RELEASE_TEST_DIR", scratch.path())
                .stdin(Stdio::null())
                .stderr(Stdio::inherit())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("the screen driver exceeded its time limit")??;
        let stdout = String::from_utf8(output.stdout)?;
        eprintln!("{driver}: {stdout}");
        let last = stdout.lines().last().context("the driver's result")?;
        let outcome: serde_json::Value = serde_json::from_str(last)?;
        anyhow::ensure!(
            output.status.success() && outcome["ok"] == serde_json::json!(true),
            "the {scenario} screen case: {outcome}"
        );
        if scenario == "take-offline" {
            anyhow::ensure!(outcome["packaging"]["count"] == 1, "one packaging row");
            anyhow::ensure!(outcome["location"]["count"] == 20002, "all location rows");
            anyhow::ensure!(outcome["reads"]["location"] == 201, "100-row location pages");
            return Ok(());
        }
        let ids: Vec<String> = serde_json::from_value(outcome["ids"].clone())?;
        let expected = if matches!(scenario, "offline" | "case2") {
            3
        } else {
            1
        };
        anyhow::ensure!(ids.len() == expected, "created ids: {outcome}");
        let rows = sqlx::query("SELECT id::text, packaging_code, row_version, location_id::text FROM wms.packaging WHERE id::text <> $1").bind(&PACKAGING_ID).fetch_all(&mut admin)
            .await?;
        anyhow::ensure!(rows.len() == expected, "exactly {expected} new database rows");
        for row in rows {
            let id: String = row.get(0);
            let code: String = row.get(1);
            let version: i32 = row.get(2);
            anyhow::ensure!(ids.contains(&id), "database row {id} came from this screen");
            anyhow::ensure!(version == 1, "row {id} was created once: row_version {version}");
            let valid_code = match scenario {
                "offline" => ["OFFLINE-1", "OFFLINE-2", "OFFLINE-3"].contains(&code.as_str()),
                "refusal" => code == "CORRECTED",
                "lost-reply" => code == "LOST-REPLY",
                "release" => code == "RELEASE",
                "old-entry" => code == "R2",
                "case2" => ["CASE2-1", "CASE2-2", "CASE2-3"].contains(&code.as_str()),
                _ => unreachable!("the test names a supported case"),
            };
            anyhow::ensure!(valid_code, "the stored packaging code is {code}");
            if scenario == "case2" {
                let location: String = row.get(3);
                anyhow::ensure!(location == LOCATION_A_ID, "row {id} uses the selected location");
            }
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    drop(admin);
    application.remove().await?;
    result
}

async fn create(application: &Application, path: &str, input: Value) -> anyhow::Result<()> {
    let answer = application.routes.post(path, input).await?;
    ensure!(answer.get("value").is_some(), "{path} refused: {answer}");
    Ok(())
}

async fn locations(application: &Application) -> anyhow::Result<()> {
    for first in (1..=20000).step_by(100) {
        let batch = (first..first+100).map(|index| {
            let id = format!("10000000-0000-4000-8000-{index:012}");
            json!({"request_id":id,"id":id,"idempotency_key":id,"location_code":format!("OFFLINE-LOC-{index:05}")})
        }).collect::<Vec<_>>();
        let routes = &application.routes;
        let response = routes
            .client
            .post(format!("{}/location/create", routes.endpoint))
            .header("Host", &routes.host)
            .bearer_auth(&routes.bearer)
            .json(&batch)
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "location creation returned {}",
            response.status()
        );
        let answer: Vec<Value> = response.json().await?;
        ensure!(
            answer.len() == batch.len() && answer.iter().all(|row| row.get("value").is_some()),
            "location creation refused: {answer:?}"
        );
    }
    Ok(())
}
