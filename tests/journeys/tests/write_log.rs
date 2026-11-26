//! Write log claims through product deployment; SQL on `platform.write_log` only observes.
//!
//! Not written here: the call bound, late completion and expiry under retention wait
//! for the Open item X4 (no deployed caller of write log retention).

mod common;

use std::time::Duration;

use anyhow::{Context as _, ensure};
use common::request::{self, until};
use common::{App, Observed};
use serde_json::{Value, json};
use tokio::time::Instant;

/// How long an observed session state takes to appear. The fixture's `widget.hold`
/// statement keeps its transaction open for 4 seconds, under the host's 5 second
/// statement timeout.
const OBSERVE: Duration = Duration::from_secs(30);
/// A session that runs the hold statement inside an open transaction.
const OPEN_HOLD: &str = "SELECT EXISTS (SELECT FROM pg_stat_activity \
     WHERE xact_start IS NOT NULL AND query LIKE '%pg_sleep%' AND pid <> pg_backend_pid())";
/// A session that waits for another transaction's ID: a competing claim insert.
const WAITING_CLAIM: &str =
    "SELECT EXISTS (SELECT FROM pg_locks WHERE locktype = 'transactionid' AND NOT granted)";
const HOLD_OPERATION: &str = "platform-fixture:widget/hold";

/// Poll a boolean observation until it holds, or fail at the deadline.
async fn observe_until(
    database: &mut Observed,
    sql: &'static str,
    what: &str,
) -> anyhow::Result<()> {
    let end = Instant::now() + OBSERVE;
    while !sqlx::query_scalar::<_, bool>(sql)
        .fetch_one(&mut database.connection)
        .await?
    {
        ensure!(Instant::now() < end, "{what} within {OBSERVE:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

/// The result texts of the write log rows of one key.
async fn logged(
    database: &mut Observed,
    operation: &str,
    key: &str,
) -> anyhow::Result<Vec<Option<String>>> {
    Ok(sqlx::query_scalar(
        "SELECT result FROM platform.write_log WHERE operation = $1 AND idempotency_key = $2",
    )
    .bind(operation)
    .bind(key)
    .fetch_all(&mut database.connection)
    .await?)
}

fn hold(key: &str) -> Value {
    json!({"request_id": key, "value": {"idempotency_key": key}})
}

common::journey! {
    /// A commit logs one row whose result is the reply. A refused call leaves no row. A
    /// generated delete answers its known result.
    async fn a_claim_commits_with_its_work_or_not_at_all() -> anyhow::Result<()> {
        let app = App::prepare(&request::fixture("platform_fixture")).await?;
        let routes = &app.routes;
        let mut database = app.env.observe().await?;

        let id = uuid::Uuid::now_v7().to_string();
        let created = routes.create("commit", "commit", &id, "standard").await?;
        ensure!(
            created.get("value").is_some(),
            "the create commits: {created}"
        );
        let rows = logged(&mut database, "platform-fixture:widget/create", "commit").await?;
        let [Some(result)] = rows.as_slice() else {
            anyhow::bail!("one committed row holds the result, not {rows:?}");
        };
        ensure!(
            serde_json::from_str::<Value>(result)? == created,
            "the logged result {result} is the reply {created}"
        );

        let version = &created["value"]["edit_version"];
        let stale = routes
            .post(
                "/widget/update",
                json!({"request_id": "stale", "id": id, "expected_edit_version": "999", "code": "priority", "maker_id": null, "note": null}),
            )
            .await?;
        ensure!(
            stale.get("error").is_some(),
            "a stale edit version is refused: {stale}"
        );
        let rows = logged(&mut database, "platform-fixture:widget/update", "stale").await?;
        ensure!(rows.is_empty(), "a refused call leaves no row: {rows:?}");

        let deleted = routes
            .post(
                "/widget/delete",
                json!({"request_id": "delete", "id": id, "expected_edit_version": version}),
            )
            .await?;
        ensure!(
            deleted["value"]["outcome"] == "deleted",
            "a delete answers deleted: {deleted}"
        );
        let rows = logged(&mut database, "platform-fixture:widget/delete", "delete").await?;
        let [Some(result)] = rows.as_slice() else {
            anyhow::bail!("the delete logs its known result, not {rows:?}");
        };
        ensure!(
            serde_json::from_str::<Value>(result)? == deleted,
            "the logged delete result {result} is the reply {deleted}"
        );
        Ok(())
    }
}

common::journey! {
    /// A second call with the same key waits for the first claim's transaction, then
    /// answers the same result, and the key logs one row.
    async fn a_competing_claim_waits_for_the_first() -> anyhow::Result<()> {
        let app = App::prepare(&request::fixture("platform_fixture")).await?;
        let routes = &app.routes;
        let mut database = app.env.observe().await?;

        let first = routes.post("/widget/hold", hold("compete"));
        let second = async {
            observe_until(&mut database, OPEN_HOLD, "the first claim is open").await?;
            let (second, ()) = tokio::try_join!(
                routes.post("/widget/hold", hold("compete")),
                observe_until(
                    &mut database,
                    WAITING_CLAIM,
                    "the second claim waits on the first"
                ),
            )?;
            anyhow::Ok(second)
        };
        let (first, second) = tokio::try_join!(first, second)?;
        ensure!(
            first == second,
            "both calls answer one result: {first} / {second}"
        );
        let rows = logged(&mut database, HOLD_OPERATION, "compete").await?;
        ensure!(rows.len() == 1, "one key logs one row: {rows:?}");
        Ok(())
    }
}

common::journey! {
    /// A host that dies inside an open claim refuses its caller, and its transaction rolls
    /// back, so the same key later answers as a new request.
    async fn a_lost_session_refuses_and_frees_its_key() -> anyhow::Result<()> {
        let app = App::prepare(&request::fixture("platform_fixture")).await?;
        let routes = &app.routes;
        let env = &app.env;
        let client = &env.org.cluster.client;
        let mut database = env.observe().await?;

        let call = routes.post("/widget/hold", hold("lost"));
        let kill = async {
            observe_until(&mut database, OPEN_HOLD, "the claim is open").await?;
            let now: String = sqlx::query_scalar("SELECT now()::text")
                .fetch_one(&mut database.connection)
                .await?;
            request::delete_host(client, env, Some(0)).await?;
            anyhow::Ok(now)
        };
        let (answer, killed) = tokio::join!(call, kill);
        let killed = killed?;
        ensure!(
            answer
                .as_ref()
                .map_or(true, |answer| answer.get("value").is_none()),
            "a lost session refuses its caller: {answer:?}"
        );

        let retry = until(
            request::RESTART_DEADLINE,
            "the key answers again",
            || async {
                Ok(routes
                    .post("/widget/hold", hold("lost"))
                    .await
                    .ok()
                    .filter(|answer| answer.get("value").is_some()))
            },
        )
        .await?;
        let fresh: bool = sqlx::query_scalar(
            "SELECT created_at > $1::timestamptz FROM platform.write_log \
             WHERE operation = $2 AND idempotency_key = 'lost'",
        )
        .bind(&killed)
        .bind(HOLD_OPERATION)
        .fetch_one(&mut database.connection)
        .await
        .context("the retry logs the key")?;
        ensure!(
            fresh,
            "the retry {retry} claims the key as new after the loss"
        );
        Ok(())
    }
}
