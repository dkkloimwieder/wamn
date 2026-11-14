//! WMS business operations over real local components and PostgreSQL.

use sqlx::Row as _;
use std::path::PathBuf;

use anyhow::Context as _;
use wamn_test_application::Application;

#[tokio::test(flavor = "multi_thread")]
async fn operations_and_replay() -> anyhow::Result<()> {
    let application: Application = panic!("wamn-eulfr.12.9: no product revision import");
    let project_url = application.database_url().await?;
    let app = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("WMS source")?
        .to_owned();
    let mut admin = wamn_postgres::connect(&project_url).await?;

    // A revision near the top of int4, with room for the moves that follow.
    let initial_revision = 2_147_483_547_i64;
    crate::business_fixture::seed_fixture(&mut admin, initial_revision).await?;
    let runtime = crate::business_fixture::runtime_phase();
    let route = crate::wms_runtime_live::Route::local(
        application.routes.endpoint.clone(),
        application.routes.host.clone(),
        application.routes.bearer.clone(),
    );
    crate::inventory_commands_live::assert_outcomes(&mut admin).await?;
    crate::inventory_commands_live::assert_fresh_reads(&mut admin, &project_url).await?;
    crate::wms_runtime_live::assert_contention_and_replay(&route, &runtime, initial_revision)
        .await?;
    crate::wms_runtime_live::assert_remaining_operations(&route, &runtime).await?;
    crate::wms_runtime_live::assert_blank_codes_refuse(&route).await?;
    // Every balance equals the sum of its transactions, after the seed and
    // every command above.
    let disagreements = sqlx::query(sqlx::AssertSqlSafe(
        (include_str!("inventory_balance.sql")).to_string(),
    ))
    .fetch_all(&mut admin)
    .await
    .context("run the inventory balance query")?;
    let disagreements = disagreements
        .iter()
        .map(|row| {
            (0..5)
                .map(|column| row.get::<Option<String>, _>(column).unwrap_or_default())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        disagreements.is_empty(),
        "every balance equals the sum of its transactions: {disagreements:?}"
    );
    drop(admin);
    application.remove().await?;
    Ok(())
}
