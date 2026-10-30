//! NetSuite assertions for the existing ignored environment kind target.

use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};

use super::integration::WorkflowEnvironment;
use super::{Run, http_connection};

pub async fn seed(project: &tokio_postgres::Client) -> anyhow::Result<()> {
    project.batch_execute("INSERT INTO rest_consumer_fixture.item (key,name,external_system_id,external_id)
        SELECT key,name,(SELECT external_system_id FROM platform.integration WHERE name='netsuite'),key
        FROM (VALUES ('1','First'),('2','Second'),('3','Third'),('4','Fourth')) AS items(key,name)").await?;
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
        records
            .iter()
            .all(|r| r["netsuite"] == true && r["basic_verified"] == true),
        "unauthenticated NetSuite request: {records:?}"
    );
    Ok(records)
}

async fn completed(
    run: &Run,
    project: &tokio_postgres::Client,
    wiring: &str,
    count: usize,
) -> anyhow::Result<Vec<Value>> {
    wamn_gate_harness::run_check::check(
        &run.work,
        "netsuite_completed",
        project,
        completed_inner(project, wiring, count),
    )
    .await
}

async fn completed_inner(
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
            Some("failed" | "infrastructure-failure")
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
    run: &Run,
    project: &tokio_postgres::Client,
    wiring: &str,
    count: usize,
) -> anyhow::Result<Vec<Value>> {
    wamn_gate_harness::run_check::check(
        &run.work,
        "netsuite_wait",
        project,
        wait_inner(run, project, wiring, count),
    )
    .await
}

async fn wait_inner(
    run: &Run,
    project: &tokio_postgres::Client,
    wiring: &str,
    count: usize,
) -> anyhow::Result<Vec<Value>> {
    let deadline = Instant::now() + Duration::from_secs(360);
    loop {
        let runs = completed(run, project, wiring, count).await?;
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
    workflows: &WorkflowEnvironment<'_>,
) -> anyhow::Result<Value> {
    wamn_gate_harness::run_check::check(
        &run.work,
        "netsuite_assertions",
        project,
        assertions_inner(run, project, workflows),
    )
    .await
}

async fn assertions_inner(
    run: &Run,
    project: &tokio_postgres::Client,
    workflows: &WorkflowEnvironment<'_>,
) -> anyhow::Result<Value> {
    let pulls = wait(run, project, "netsuite_pull", 1).await?;
    // One pull is enough. The admin disables the schedule, so no later pull
    // adds requests to the log.
    super::integration::disable_schedule(workflows, "netsuite", "netsuite_pull").await?;
    let pull = &pulls[0];
    let due = pull["input_json"]["due"].as_str().context("schedule due")?;
    ensure!(
        pull["trigger_source"] == "schedule"
            && pull["invoked_by"] == "schedule"
            && pull["execution_principal_id"] == wamn_project_state::workflow_user_id().to_string()
            && pull["execution_role"] == super::integration::PULL_ROLE
            && pull["manifest_digest"] == workflows.digest
            && pull["idempotency_key"] == format!("schedule:netsuite/netsuite_pull:{due}"),
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
            "SELECT jsonb_agg(jsonb_build_object('key',key,'name',name) ORDER BY key) \
             FROM rest_consumer_fixture.item",
            &[],
        )
        .await?
        .get(0);
    ensure!(
        items
            == json!([{"key":"1","name":"First"},{"key":"2","name":"Second"},{"key":"3","name":"Third"},{"key":"4","name":"Fourth"}]),
        "consumer rows differ: {items}"
    );
    let provenance: bool = project
        .query_one(
            "SELECT bool_and(external_id = key AND external_system_id = \
         (SELECT external_system_id FROM platform.integration WHERE name = 'netsuite')) \
         FROM rest_consumer_fixture.item",
            &[],
        )
        .await?
        .get(0);
    ensure!(provenance, "consumer rows lost their NetSuite provenance");
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
    project
        .execute(
            "INSERT INTO rest_consumer_fixture.item (key,name,external_system_id,external_id)
        VALUES ('push-one','Pushed customer',
        (SELECT external_system_id FROM platform.integration WHERE name='netsuite'),'push-one')",
            &[],
        )
        .await?;
    let pushes = wait(run, project, "netsuite_push", 1).await?;
    // An event trigger is a workflow row, so the run names it by its wiring
    // id and records its CDC delivery as the invoker.
    ensure!(
        pushes[0]["trigger_source"] == "event"
            && pushes[0]["service_principal_id"].is_null()
            && pushes[0]["registration_id"].is_null()
            && pushes[0]["invoked_by"]
                .as_str()
                .is_some_and(|delivery| delivery.starts_with("event:"))
            && pushes[0]["execution_principal_id"]
                == wamn_project_state::workflow_user_id().to_string()
            && pushes[0]["execution_role"] == "connector-push",
        "wrong push identity: {pushes:?}"
    );
    // A scheduled pull can land again while the push waits, so only the PUTs count.
    let first = requests(run).await?;
    let first: Vec<_> = first.into_iter().filter(|r| r["method"] == "PUT").collect();
    ensure!(
        first.len() == 1,
        "one insert must send exactly one PUT: {first:?}"
    );
    assert_put(&first[0])?;

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../apps/netsuite/publication/wirings/netsuite_push.json");
    let mut document = wamn_control::author_workflow::read_wiring_document(&path)?;
    "netsuite_push_replay".clone_into(&mut document.wiring_id);
    super::integration::enable_trigger(
        workflows,
        "netsuite",
        &document,
        "connector-replay",
        &[],
        wamn_control::author_workflow::WorkflowTrigger::Manual,
        None,
    )
    .await?;
    let replay = super::integration::start_workflow(
        &run.work,
        workflows,
        "netsuite",
        &document.wiring_id,
        &pushes[0]["input_json"],
        "netsuite-fixture-redelivery",
    )
    .await?;
    wait(run, project, &document.wiring_id, 1).await?;
    let records = requests(run).await?;
    let records: Vec<_> = records
        .into_iter()
        .filter(|r| r["method"] == "PUT")
        .collect();
    ensure!(
        records.len() == 2,
        "unexpected request after redelivery: {records:?}"
    );
    assert_put(&records[1])?;
    ensure!(
        records[0]["target"] == records[1]["target"]
            && records[0]["location"] == records[1]["location"],
        "retry changed the external-id path"
    );
    // The pull runs every minute, so more than one run can exist by now.
    let pulls: Vec<Value> = project
        .query("SELECT to_jsonb(r) FROM wamn_run.runs r WHERE package_id='netsuite' AND wiring_id='netsuite_pull'", &[])
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    ensure!(
        pulls.iter().all(|r| !matches!(
            r["status"].as_str(),
            Some("failed" | "infrastructure-failure")
        )) && pulls.iter().any(|r| r["status"] == "completed"),
        "a pull did not complete: {pulls:?}"
    );
    Ok(
        json!({"requests":records,"consumer":items,"received":received,"last_synced_at":checkpoint,"pull":pull,"replay":replay}),
    )
}

fn assert_put(record: &Value) -> anyhow::Result<()> {
    ensure!(
        record["method"] == "PUT"
            && record["target"].as_str().is_some_and(
                |t| unreserved_decoded(t) == "/services/rest/record/v1/customer/eid:push-one"
            )
            && record["json"] == json!({"entityId":"Pushed customer","externalId":"push-one"})
            && record["status"] == "204 No Content"
            && record["location"]
                .as_str()
                .is_some_and(|v| unreserved_decoded(v).ends_with("/customer/eid:push-one")),
        "wrong NetSuite push: {record}"
    );
    Ok(())
}

/// The target with each percent-encoded unreserved character decoded
/// (RFC 3986 6.2.2.2). jsonata-core's $encodeUrlComponent encodes - . _ ~,
/// and the two forms name the same resource.
fn unreserved_decoded(target: &str) -> String {
    let bytes = target.as_bytes();
    let mut out = String::with_capacity(target.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&target[i + 1..i + 3], 16)
            && (byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
        {
            out.push(byte as char);
            i += 3;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}
