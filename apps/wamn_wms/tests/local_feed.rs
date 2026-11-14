//! The packaging screen follows the deployed WMS change feed
//! (docs/architecture/execution.md#change-feed).
//!
//! The test serves WMS over real components and PostgreSQL, and runs
//! `web/sync-bench/bench/follow.mjs`, which opens the packaging screen in
//! headless Chrome. The screen lists once. A create through the host's route
//! then reaches it through the feed with no second list read, and a row that
//! the test deletes in the database leaves it. The selected runner prepares
//! the screen and supplies its path.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::Context as _;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use wamn_integration_tests::product_application::Application;

use crate::business_fixture::{LOCATION_A_ID, PACKAGING_ID};

#[tokio::test(flavor = "multi_thread")]
async fn the_packaging_screen_follows_a_write_and_a_delete() -> anyhow::Result<()> {
    let application: Application = panic!("wamn-eulfr.12.9: no product packaging delete");
    let project_url = application.database_url().await?;
    let app = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("WMS source")?
        .to_owned();
    let mut admin = wamn_postgres::connect(&project_url).await?;

    let result = async {
        let audience = application.audience().await?;
        let bench = app.join("../../web/sync-bench").canonicalize()?;
        let mut driver = tokio::process::Command::new("node")
            .arg("bench/follow.mjs")
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
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("start the screen driver")?;
        let mut stdin = driver.stdin.take().context("the driver's stdin")?;
        let mut lines = BufReader::new(driver.stdout.take().context("the driver's stdout")?).lines();
        let mut last = String::new();
        while let Some(line) = lines.next_line().await? {
            eprintln!("follow.mjs: {line}");
            if let Some(id) = line.strip_prefix("DELETE ") {
                // A delete with no operation route: the test is the writer,
                // and the history entry names it.
                sqlx::raw_sql(sqlx::AssertSqlSafe((&format!(
                        "BEGIN;\n\
                         SELECT set_config('app.user_id', '00000000-0000-4000-8000-0000000000f1', true), \
                                set_config('app.operation', 'admin:follow-delete', true);\n\
                         DELETE FROM wms.packaging WHERE id = '{id}';\n\
                         COMMIT;"
                    )).to_string())).execute(&mut admin).await.map(drop)
                    .context("delete the created row")?;
                stdin.write_all(b"deleted\n").await?;
                stdin.flush().await?;
            }
            last = line;
        }
        let status = driver.wait().await?;
        let outcome: serde_json::Value =
            serde_json::from_str(&last).with_context(|| format!("the driver's result: {last}"))?;
        anyhow::ensure!(
            status.success() && outcome["ok"] == serde_json::json!(true),
            "the screen follows the write and the delete: {outcome}"
        );
        anyhow::ensure!(
            outcome["reads"]["list"] == serde_json::json!(1),
            "the screen lists once: {outcome}"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    drop(admin);
    application.remove().await?;
    result
}
