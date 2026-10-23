//! NetSuite assertions for the existing ignored environment kind target.

use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_workflow::{PostgresWorkflows, StartRequest, Trigger, Workflows as _};

use super::{Run, http_connection};

pub async fn seed(project: &tokio_postgres::Client) -> anyhow::Result<()> {
    project.batch_execute("INSERT INTO rest_consumer_fixture.item (key,name) VALUES ('1','First'),('2','Second'),('3','Third'),('4','Fourth')").await?;
    // These are initial state, not push events. No reader is running yet.
    // Advance only the private project's inactive slot past the seed commit.
    let slots = project.query("SELECT slot_name FROM pg_replication_slots WHERE database=current_database() AND NOT active", &[]).await?;
    ensure!(slots.len() == 1, "expected one inactive fixture CDC slot");
    let slot: String = slots[0].get(0);
    project
        .query_one(
            "SELECT end_lsn::text FROM pg_replication_slot_advance($1, pg_current_wal_lsn())",
            &[&slot],
        )
        .await?;
    Ok(())
}

async fn requests(run: &Run) -> anyhow::Result<Vec<Value>> {
    let records = http_connection::request_records(&http_connection::request_log(run).await?)?;
    ensure!(
        records.iter().all(|r| r["netsuite"] == true
            && r["oauth1_verified"] == true
            && r["realm"].as_str().is_some_and(|v| !v.is_empty())),
        "unsigned or wrong-realm NetSuite request: {records:?}"
    );
    Ok(records)
}

async fn completed(
    project: &tokio_postgres::Client,
    wiring: &str,
    count: usize,
) -> anyhow::Result<Vec<Value>> {
    let rows = project.query("SELECT to_jsonb(r) FROM wamn_run.runs r WHERE package_id='netsuite' AND wiring_id=$1 ORDER BY created_at", &[&wiring]).await?;
    let runs: Vec<Value> = rows.iter().map(|r| r.get(0)).collect();
    ensure!(
        runs.len() <= count,
        "unexpected extra {wiring} run: {runs:?}"
    );
    ensure!(
        runs.iter().all(|r| !matches!(
            r["status"].as_str(),
            Some("failed" | "infrastructure-failure" | "effect-uncertain")
        )),
        "{wiring} failed: {runs:?}"
    );
    if runs.len() == count && runs.iter().all(|r| r["status"] == "completed") {
        Ok(runs)
    } else {
        Ok(Vec::new())
    }
}

async fn wait(
    project: &tokio_postgres::Client,
    wiring: &str,
    count: usize,
) -> anyhow::Result<Vec<Value>> {
    let deadline = Instant::now() + Duration::from_secs(360);
    loop {
        let runs = completed(project, wiring, count).await?;
        if !runs.is_empty() {
            return Ok(runs);
        }
        ensure!(
            Instant::now() < deadline,
            "{wiring} did not complete in 360 seconds"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub async fn assertions(
    run: &Run,
    project: &tokio_postgres::Client,
    project_url: &str,
    tenant: &str,
    principal: &str,
    digest: &str,
) -> anyhow::Result<Value> {
    let pulls = wait(project, "netsuite_pull", 1).await?;
    let pull = &pulls[0];
    let due = pull["input_json"]["due"].as_str().context("schedule due")?;
    ensure!(
        pull["trigger_source"] == "schedule"
            && pull["service_principal_id"] == principal
            && pull["manifest_digest"] == digest
            && pull["idempotency_key"] == format!("schedule:netsuite/pull:{due}"),
        "wrong pull identity: {pull}"
    );
    let checkpoint: Value = project
        .query_one(
            "SELECT value FROM wamn_integration.setting WHERE key='last_synced_at'",
            &[],
        )
        .await?
        .get(0);
    let advanced: bool = project
        .query_one(
            "SELECT (value #>> '{}')::timestamptz = $1::text::timestamptz \
               FROM wamn_integration.setting WHERE key='last_synced_at'",
            &[&due],
        )
        .await?
        .get(0);
    ensure!(
        advanced,
        "checkpoint did not advance to due {due}: {checkpoint}"
    );
    let items: Value = project
        .query_one(
            "SELECT jsonb_agg(to_jsonb(i) ORDER BY key) FROM rest_consumer_fixture.item i",
            &[],
        )
        .await?
        .get(0);
    ensure!(
        items
            == json!([{"key":"1","name":"First"},{"key":"2","name":"Second"},{"key":"3","name":"Third"},{"key":"4","name":"Fourth"}]),
        "consumer rows differ: {items}"
    );
    // Since the rows predate capture, prove receipt through the actual command result.
    let result: String = project.query_one("SELECT result FROM platform.write_log WHERE operation='rest-consumer-fixture:item/record' AND idempotency_key=$1", &[&due]).await?.get(0);
    let received: Value = serde_json::from_str(&result)?;
    ensure!(
        received["value"]["keys"] == json!(["1", "2", "3", "4"]),
        "consumer did not receive every pulled row: {received}"
    );
    let pages = requests(run).await?;
    ensure!(
        pages.len() == 2,
        "expected exactly two pages, without an empty third page: {pages:?}"
    );
    for (page, offset) in pages.iter().zip([0, 2]) {
        let url = url::Url::parse(&format!(
            "{}{}",
            http_connection::ORIGIN,
            page["target"].as_str().context("target")?
        ))?;
        let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        ensure!(
            page["method"] == "POST"
                && url.path() == "/services/rest/query/v1/suiteql"
                && query.get("offset") == Some(&offset.to_string())
                && query.get("limit").map(String::as_str) == Some("2")
                && page["headers"]["prefer"] == "transient"
                && page["json"] == pages[0]["json"]
                && page["json"]["q"]
                    .as_str()
                    .is_some_and(|q| q.contains("lastmodifieddate") && q.ends_with("ORDER BY id"))
                && page["status"] == "200 OK"
                && page["response"]["items"]
                    .as_array()
                    .is_some_and(|v| v.len() == 2)
                && page["response"]["hasMore"] == (offset == 0),
            "wrong SuiteQL page: {page}"
        );
    }
    project.execute("INSERT INTO rest_consumer_fixture.item (key,name) VALUES ('push-one','Pushed customer')", &[]).await?;
    let pushes = wait(project, "netsuite_push", 1).await?;
    ensure!(
        pushes[0]["trigger_source"] == "event" && pushes[0]["service_principal_id"] == principal,
        "wrong push identity: {pushes:?}"
    );
    let first = requests(run).await?;
    ensure!(
        first.len() == 3,
        "one insert must send exactly one PUT: {first:?}"
    );
    assert_put(&first[2])?;

    // Replay the exact delivered input through the released wiring. A new run
    // key forces a second HTTP call rather than merely returning the first run.
    let (client, connection) = tokio_postgres::connect(project_url, tokio_postgres::NoTls).await?;
    let task = tokio::spawn(connection);
    let workflows = PostgresWorkflows::new(client, "wamn_run", tenant, "integration");
    let replay = workflows
        .start(&StartRequest {
            manifest_digest: digest.into(),
            package_id: "netsuite".into(),
            wiring_id: "netsuite_push".into(),
            wiring_version: 1,
            idempotency_key: "netsuite-fixture-redelivery".into(),
            input: pushes[0]["input_json"].clone(),
            trigger: Trigger::Automation {
                service_principal_id: principal.into(),
            },
        })
        .await?;
    wait(project, "netsuite_push", 2).await?;
    let records = requests(run).await?;
    ensure!(
        records.len() == 4,
        "unexpected request after redelivery: {records:?}"
    );
    assert_put(&records[3])?;
    ensure!(
        records[2]["target"] == records[3]["target"]
            && records[2]["location"] == records[3]["location"],
        "retry changed the external-id path"
    );
    ensure!(
        completed(project, "netsuite_pull", 1).await?.len() == 1,
        "expected one pull"
    );
    task.abort();
    Ok(
        json!({"requests":records,"consumer":items,"received":received,"last_synced_at":checkpoint,"pull":pull,"replay":replay}),
    )
}

fn assert_put(record: &Value) -> anyhow::Result<()> {
    ensure!(
        record["method"] == "PUT"
            && record["target"] == "/services/rest/record/v1/customer/eid:push-one"
            && record["json"] == json!({"entityId":"Pushed customer","externalId":"push-one"})
            && record["status"] == "204 No Content"
            && record["location"]
                .as_str()
                .is_some_and(|v| v.ends_with("/customer/eid:push-one")),
        "wrong NetSuite push: {record}"
    );
    Ok(())
}
