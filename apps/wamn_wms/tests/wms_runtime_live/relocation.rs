//! Packaging relocation through real local components and PostgreSQL.

use super::{OCCURRED_AT, Route, business_snapshot, refused_without_change, value};
use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_gate_harness::journey::RuntimePhase;

const HELD: &str = "00000000-0000-0000-0000-000000000302";
const CLOSED: &str = "00000000-0000-0000-0000-000000000303";
const PATH: &str = "/packaging/relocate";

fn command(key: &str, packaging: &str, location: &str, revision: i64) -> Value {
    json!({"idempotency_key":key,"packaging_id":packaging,
        "to_location_id":location,"expected_row_version":revision,"occurred_at":OCCURRED_AT})
}

async fn invoke(route: &Route, path: &str, command: &Value) -> anyhow::Result<Value> {
    let answer = route
        .post(
            &reqwest::Client::new(),
            path,
            &json!([{"request_id":"relocation","value":command}]),
        )
        .await?;
    Ok(value(&answer, "relocation")?.clone())
}

async fn inventory(admin: &tokio_postgres::Client) -> anyhow::Result<Vec<Value>> {
    Ok(admin
        .query("SELECT to_jsonb(i) FROM wms.inventory i ORDER BY id", &[])
        .await?
        .into_iter()
        .map(|r| r.get(0))
        .collect())
}

async fn history(admin: &tokio_postgres::Client, operation: &Value) -> anyhow::Result<Vec<Value>> {
    let id = operation.as_str().context("operation identity")?;
    Ok(admin.query("SELECT to_jsonb(t) FROM wms.inventory_transaction t WHERE operation_id=$1::text::uuid ORDER BY inventory_id", &[&id])
        .await?.into_iter().map(|r| r.get(0)).collect())
}

pub(crate) async fn assert_relocation(
    route: &Route,
    runtime: &RuntimePhase,
    admin: &tokio_postgres::Client,
) -> anyhow::Result<()> {
    use crate::business_fixture::{INVENTORY_ID, LOCATION_A_ID, PACKAGING_A_ID, PACKAGING_B_ID};

    admin.batch_execute("BEGIN; SELECT set_config('app.user_id','00000000-0000-4000-8000-0000000000f1',true), set_config('app.operation','admin:seed-relocation',true)").await?;
    admin.execute("INSERT INTO wms.inventory(id,product_id,packaging_id,location_id,quantity,disposition,lifecycle) SELECT $1::text::uuid,product_id,packaging_id,location_id,4,'held','open' FROM wms.inventory WHERE id=$2::text::uuid", &[&HELD,&INVENTORY_ID]).await?;
    admin.execute("INSERT INTO wms.inventory(id,product_id,packaging_id,location_id,quantity,disposition,lifecycle) SELECT $1::text::uuid,product_id,packaging_id,location_id,0,'available','closed' FROM wms.inventory WHERE id=$2::text::uuid", &[&CLOSED,&INVENTORY_ID]).await?;
    admin.batch_execute("COMMIT").await?;
    admin.batch_execute("SET TIME ZONE 'UTC'").await?;
    let original = inventory(admin).await?;
    let relocation = command("relocate-first", PACKAGING_A_ID, &runtime.to_location_id, 1);

    // The larger identity is inserted second in the ordered history loop.
    admin.batch_execute(&format!("ALTER TABLE wms.inventory_transaction ADD CONSTRAINT test_reject_relocation_second CHECK (type <> 'packaging_relocate' OR inventory_id <> '{HELD}'::uuid) NOT VALID")).await?;
    let attempted =
        refused_without_change(route, admin, PATH, relocation.clone(), "internal_error").await;
    admin
        .batch_execute(
            "ALTER TABLE wms.inventory_transaction DROP CONSTRAINT test_reject_relocation_second",
        )
        .await?;
    attempted?;

    let result = invoke(route, PATH, &relocation).await?;
    anyhow::ensure!(
        result["packaging_id"] == PACKAGING_A_ID
            && result["location_id"] == runtime.to_location_id
            && result["row_version"] == 2
            && result["type"] == "tote"
            && result["code"] == "PKG-A"
            && result["lifecycle"] == "open",
        "relocation returns the committed packaging: {result}"
    );
    let moved = inventory(admin).await?;
    let transactions = history(admin, &result["operation_id"]).await?;
    anyhow::ensure!(
        transactions.len() == 2,
        "both open identities have history: {transactions:?}"
    );
    for (from, to) in original.iter().zip(&moved) {
        if from["lifecycle"] == "closed" {
            anyhow::ensure!(from == to, "closed inventory does not move");
            continue;
        }
        for field in [
            "id",
            "product_id",
            "packaging_id",
            "quantity",
            "disposition",
            "lifecycle",
        ] {
            anyhow::ensure!(from[field] == to[field], "relocation preserves {field}");
        }
        anyhow::ensure!(
            to["location_id"] == runtime.to_location_id
                && to["row_version"].as_i64() == from["row_version"].as_i64().map(|v| v + 1),
            "relocation explicitly updates each inventory location and revision"
        );
        let transaction = transactions
            .iter()
            .find(|t| t["inventory_id"] == from["id"])
            .context("one transaction for each open inventory")?;
        anyhow::ensure!(
            transaction["type"] == "packaging_relocate"
                && transaction["from_inventory_id"] == from["id"]
                && transaction["to_inventory_id"] == from["id"]
                && transaction["occurred_at"] == "2026-09-05T12:00:00+00:00"
                && transaction["reason"].is_null(),
            "transaction retains operation and identity: {transaction}"
        );
        for field in [
            "product_id",
            "packaging_id",
            "location_id",
            "quantity",
            "disposition",
            "lifecycle",
        ] {
            anyhow::ensure!(
                transaction[format!("from_{field}")] == from[field]
                    && transaction[format!("to_{field}")] == to[field],
                "transaction records exact {field} transition: {transaction}"
            );
        }
    }
    let committed = business_snapshot(admin).await?;
    anyhow::ensure!(
        invoke(route, PATH, &relocation).await? == result,
        "exact replay returns the original result"
    );
    anyhow::ensure!(
        business_snapshot(admin).await? == committed,
        "replay has no business effect"
    );
    for (body, code) in [
        (
            command("same-location", PACKAGING_A_ID, &runtime.to_location_id, 2),
            "invalid_input",
        ),
        (
            command("stale", PACKAGING_A_ID, LOCATION_A_ID, 1),
            "concurrency_conflict",
        ),
        (
            command(
                "missing-location",
                PACKAGING_A_ID,
                "00000000-0000-0000-0000-000000000299",
                2,
            ),
            "not_found",
        ),
        (
            command(
                "missing-packaging",
                "00000000-0000-0000-0000-000000000599",
                LOCATION_A_ID,
                1,
            ),
            "not_found",
        ),
        (
            command("relocate-first", PACKAGING_A_ID, LOCATION_A_ID, 1),
            "idempotency_conflict",
        ),
    ] {
        refused_without_change(route, admin, PATH, body, code).await?;
    }

    let later = command("relocate-back", PACKAGING_A_ID, LOCATION_A_ID, 2);
    let later_result = invoke(route, PATH, &later).await?;
    anyhow::ensure!(
        later_result["row_version"] == 3,
        "later relocation advances packaging revision"
    );
    let adjust = json!({"idempotency_key":"adjust-after-relocation","inventory_id":HELD,
        "to_quantity":"5","reason":"cycle-count","expected_row_version":3,"occurred_at":OCCURRED_AT});
    invoke(route, "/inventory/adjust", &adjust).await?;
    let later_state = business_snapshot(admin).await?;
    anyhow::ensure!(
        invoke(route, PATH, &relocation).await? == result,
        "later inventory and packaging changes do not change the replay result"
    );
    anyhow::ensure!(
        history(admin, &result["operation_id"]).await? == transactions,
        "later operations preserve the earlier immutable transactions"
    );
    anyhow::ensure!(
        business_snapshot(admin).await? == later_state,
        "old replay does not change later state"
    );

    let competing = [
        json!([{"request_id":"a","value":command("competing-a", PACKAGING_A_ID, &runtime.to_location_id, 3)}]),
        json!([{"request_id":"b","value":command("competing-b", PACKAGING_A_ID, &runtime.to_location_id, 3)}]),
    ];
    let client = reqwest::Client::new();
    let (a, b) = tokio::join!(
        route.post(&client, PATH, &competing[0]),
        route.post(&client, PATH, &competing[1])
    );
    let answers = [a?, b?];
    let winner = answers
        .iter()
        .find_map(|a| a[0].get("value"))
        .context("one relocation wins")?;
    anyhow::ensure!(
        answers
            .iter()
            .filter(|a| a[0]["error"]["code"] == "concurrency_conflict")
            .count()
            == 1,
        "competing relocations have one revision conflict: {answers:?}"
    );
    anyhow::ensure!(
        winner["row_version"] == 4 && winner["location_id"] == runtime.to_location_id,
        "the winning relocation commits one packaging transition"
    );
    anyhow::ensure!(
        history(admin, &winner["operation_id"]).await?.len() == 2,
        "the winning relocation records both open inventory identities"
    );
    for row in inventory(admin).await? {
        let expected = if row["lifecycle"] == "open" {
            runtime.to_location_id.as_str()
        } else {
            LOCATION_A_ID
        };
        anyhow::ensure!(
            row["location_id"] == expected,
            "concurrent relocation preserves open co-location and closed history"
        );
    }

    let empty = command("empty-relocation", PACKAGING_B_ID, LOCATION_A_ID, 1);
    let empty_result = invoke(route, PATH, &empty).await?;
    anyhow::ensure!(
        empty_result["location_id"] == LOCATION_A_ID && empty_result["row_version"] == 2,
        "empty packaging relocates and advances revision"
    );
    anyhow::ensure!(
        history(admin, &empty_result["operation_id"])
            .await?
            .is_empty(),
        "empty relocation creates no inventory transactions"
    );
    invoke(
        route,
        "/packaging/close",
        &json!({"idempotency_key":"close-empty",
        "packaging_id":PACKAGING_B_ID,"expected_row_version":2}),
    )
    .await?;
    refused_without_change(
        route,
        admin,
        PATH,
        command(
            "closed-packaging",
            PACKAGING_B_ID,
            &runtime.to_location_id,
            3,
        ),
        "invalid_input",
    )
    .await?;
    let closed_state = business_snapshot(admin).await?;
    anyhow::ensure!(
        invoke(route, PATH, &empty).await? == empty_result,
        "empty relocation replays its original open result after closure"
    );
    anyhow::ensure!(
        business_snapshot(admin).await? == closed_state,
        "empty replay preserves the closed state"
    );
    membership_change_refuses_partial_relocation(route, runtime, admin).await?;
    Ok(())
}

async fn wait_for_lock(admin: &tokio_postgres::Client, sql_fragment: &str) -> anyhow::Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            admin.simple_query("SELECT pg_stat_clear_snapshot()").await?;
            let waiting: bool = admin.query_one(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND wait_event_type='Lock' AND query LIKE '%' || $1 || '%')",
                &[&sql_fragment],
            ).await?.get(0);
            if waiting {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.with_context(|| format!("wait for blocked statement containing {sql_fragment}"))??;
    Ok(())
}

fn spawn_command(
    route: &Route,
    path: &'static str,
    command: Value,
) -> tokio::task::JoinHandle<anyhow::Result<Value>> {
    let route = Route::local(
        route.endpoint.clone(),
        route.host.clone(),
        route.bearer.clone(),
    );
    tokio::spawn(async move {
        route
            .post(
                &reqwest::Client::new(),
                path,
                &json!([{"request_id":"membership","value":command}]),
            )
            .await
    })
}

async fn membership_change_refuses_partial_relocation(
    route: &Route,
    runtime: &RuntimePhase,
    admin: &tokio_postgres::Client,
) -> anyhow::Result<()> {
    use crate::business_fixture::{INVENTORY_ID, LOCATION_A_ID, PACKAGING_A_ID};
    let from = business_snapshot(admin).await?;
    let source = from["inventory"]
        .as_array()
        .context("inventory snapshot")?
        .iter()
        .find(|row| row["id"] == INVENTORY_ID)
        .context("split source")?;
    let split = json!({"idempotency_key":"membership-split","from_inventory_id":INVENTORY_ID,
        "quantity":"1","to_packaging_id":PACKAGING_A_ID,"to_location_id":runtime.to_location_id,
        "expected_row_version":source["row_version"],"occurred_at":OCCURRED_AT});
    let relocation = command("membership-relocation", PACKAGING_A_ID, LOCATION_A_ID, 4);
    admin.batch_execute("BEGIN").await?;
    admin
        .query_one(
            "SELECT id FROM wms.packaging WHERE id=$1::text::uuid FOR UPDATE",
            &[&PACKAGING_A_ID],
        )
        .await?;
    let split_task = spawn_command(route, "/inventory/split", split);
    let split_waiting = wait_for_lock(admin, "FROM packaging WHERE id IN").await;
    if let Err(error) = split_waiting {
        admin.batch_execute("ROLLBACK").await?;
        split_task.await??;
        return Err(error);
    }
    // Split holds the source inventory while waiting for packaging. Relocation
    // starts its member scan now, so its snapshot cannot include the new child.
    let relocation_task = spawn_command(route, PATH, relocation.clone());
    let relocation_waiting = wait_for_lock(admin, "FROM inventory WHERE packaging_id").await;
    admin.batch_execute("ROLLBACK").await?;
    let split_answer = split_task.await??;
    let relocation_answer = relocation_task.await??;
    relocation_waiting?;
    let split_result = value(&split_answer, "membership")?;
    super::refusal(&relocation_answer, "membership", "retry")?;
    let to = business_snapshot(admin).await?;
    anyhow::ensure!(
        to["packaging"] == from["packaging"] && to["relocate"] == from["relocate"],
        "membership drift leaves packaging and relocation claims unchanged"
    );
    let from_history = from["history"]
        .as_array()
        .context("existing transaction history")?;
    let to_history = to["history"]
        .as_array()
        .context("resulting transaction history")?;
    anyhow::ensure!(
        to_history.len() == from_history.len() + 2
            && from_history.iter().all(|row| to_history.contains(row))
            && to_history
                .iter()
                .filter(|row| row["operation_id"] == split_result["operation_id"])
                .count()
                == 2,
        "only the competing split appends history"
    );
    for row in to["inventory"].as_array().context("resulting inventory")? {
        let expected = if row["lifecycle"] == "open" {
            runtime.to_location_id.as_str()
        } else {
            LOCATION_A_ID
        };
        anyhow::ensure!(
            row["location_id"] == expected,
            "refused relocation moves no inventory"
        );
    }
    let result = invoke(route, PATH, &relocation).await?;
    anyhow::ensure!(
        history(admin, &result["operation_id"]).await?.len() == 3,
        "retry includes all three current open inventory identities"
    );
    for row in inventory(admin).await? {
        anyhow::ensure!(
            row["location_id"] == LOCATION_A_ID,
            "retry explicitly moves all open inventory while closed inventory stays at its historical location"
        );
    }
    Ok(())
}
