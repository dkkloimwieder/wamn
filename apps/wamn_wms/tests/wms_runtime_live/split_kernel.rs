//! Decimal split state and history through the production operation.

use super::{OCCURRED_AT, Route, business_snapshot, read_value, value};
use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_gate_harness::journey::RuntimePhase;

#[path = "../../data/src/inventory_split/decision.rs"]
mod decision;

const SOURCE: &str = "00000000-0000-0000-0000-000000000304";
const KEY: &str = "decimal-kernel-split";

async fn inventory(route: &Route, client: &reqwest::Client, id: &str) -> anyhow::Result<Value> {
    let answer = route
        .get(client, "/inventory/get", &json!({"id":id}))
        .await?;
    Ok(read_value(&answer)?.clone())
}

async fn history(admin: &tokio_postgres::Client, operation: &str) -> anyhow::Result<Vec<Value>> {
    Ok(admin.query(
        "SELECT to_jsonb(t) || jsonb_build_object('from_quantity',from_quantity::text,'to_quantity',to_quantity::text) FROM wms.inventory_transaction t WHERE operation_id=$1::text::uuid ORDER BY inventory_id",
        &[&operation],
    ).await?.into_iter().map(|row| row.get(0)).collect())
}

pub(crate) async fn assert_decimal_split(
    route: &Route,
    runtime: &RuntimePhase,
    admin: &tokio_postgres::Client,
) -> anyhow::Result<()> {
    assert_numeric_parity(admin).await?;
    let client = reqwest::Client::new();
    admin.batch_execute("BEGIN; SELECT set_config('app.user_id','00000000-0000-4000-8000-0000000000f1',true), set_config('app.operation','admin:seed-decimal-split',true)").await?;
    admin.execute(
        "INSERT INTO wms.inventory(id,product_id,packaging_id,location_id,quantity,disposition) SELECT $1::text::uuid,product_id,packaging_id,location_id,12.3400,'held' FROM wms.inventory WHERE id=$2::text::uuid",
        &[&SOURCE, &runtime.inventory_id],
    ).await?;
    admin.batch_execute("COMMIT").await?;
    let original = inventory(route, &client, SOURCE).await?;
    let split = json!([{"request_id":KEY,"value":{
        "idempotency_key":KEY,"from_inventory_id":SOURCE,"quantity":"0.25",
        "to_packaging_id":runtime.to_packaging_id,"to_location_id":runtime.to_location_id,
        "expected_row_version":1,"occurred_at":OCCURRED_AT}}]);
    let answer = route.post(&client, "/inventory/split", &split).await?;
    let result = value(&answer, KEY)?.clone();
    let child_id = result["new_inventory_id"]
        .as_str()
        .context("split child identity")?;
    let operation = result["operation_id"]
        .as_str()
        .context("split operation identity")?;
    let source = inventory(route, &client, SOURCE).await?;
    let child = inventory(route, &client, child_id).await?;
    anyhow::ensure!(
        result["quantity"] == "12.0900"
            && source["quantity"] == "12.0900"
            && child["quantity"] == "0.25"
            && source["row_version"] == 2
            && child["row_version"] == 1,
        "split preserves exact decimal arithmetic and scale: {source} / {child}"
    );
    for row in [&source, &child] {
        anyhow::ensure!(
            row["product_id"] == original["product_id"]
                && row["disposition"] == "held"
                && row["lifecycle"] == "open",
            "both split identities preserve product, disposition, and lifecycle: {row}"
        );
    }
    let transactions = history(admin, operation).await?;
    anyhow::ensure!(
        transactions.len() == 2,
        "split writes exactly two transactions"
    );
    for (id, to, from) in [(SOURCE, &source, Some(&original)), (child_id, &child, None)] {
        let transaction = transactions
            .iter()
            .find(|row| row["inventory_id"] == id)
            .context("split records each affected identity")?;
        anyhow::ensure!(
            transaction["type"] == "split"
                && transaction["from_inventory_id"] == SOURCE
                && transaction["to_inventory_id"] == id
                && transaction["reason"].is_null(),
            "split history retains source lineage: {transaction}"
        );
        for field in [
            "product_id",
            "packaging_id",
            "location_id",
            "quantity",
            "disposition",
            "lifecycle",
        ] {
            let expected_from = from.map_or_else(
                || {
                    if field == "quantity" {
                        json!("0")
                    } else {
                        Value::Null
                    }
                },
                |row| row[field].clone(),
            );
            anyhow::ensure!(
                transaction[format!("from_{field}")] == expected_from
                    && transaction[format!("to_{field}")] == to[field],
                "split history retains exact {field} values: {transaction}"
            );
        }
    }
    let stored: Value = admin
        .query_one(
            "SELECT result::jsonb FROM wms.inventory_split_command WHERE idempotency_key=$1",
            &[&KEY],
        )
        .await?
        .get(0);
    anyhow::ensure!(
        stored == result,
        "split stores its complete original result"
    );

    for (id, revision, quantity) in [(SOURCE, 2, "9.8765"), (child_id, 1, "1.2345")] {
        let adjust = json!([{"request_id":"later-decimal","value":{
            "idempotency_key":format!("later-decimal-{id}"),"inventory_id":id,
            "to_quantity":quantity,"reason":"cycle-count","expected_row_version":revision,
            "occurred_at":OCCURRED_AT}}]);
        let answer = route.post(&client, "/inventory/adjust", &adjust).await?;
        anyhow::ensure!(
            value(&answer, "later-decimal")?["quantity"] == quantity,
            "later adjustment commits for {id}"
        );
    }
    let later = business_snapshot(admin).await?;
    let replay = route.post(&client, "/inventory/split", &split).await?;
    anyhow::ensure!(
        value(&replay, KEY)? == &result,
        "split replay returns its original result after both quantities change"
    );
    anyhow::ensure!(
        history(admin, operation).await? == transactions,
        "later changes preserve both original split transactions"
    );
    anyhow::ensure!(
        business_snapshot(admin).await? == later,
        "split replay preserves later inventory, history, and stored claims"
    );
    Ok(())
}

// Special stored values retain PostgreSQL compatibility outside the finite proof domain.
async fn assert_numeric_parity(admin: &tokio_postgres::Client) -> anyhow::Result<()> {
    for (quantity, requested) in [
        ("12.3400", "0.25"),
        ("10.0", "0.0001"),
        ("1.000", "0.001"),
        ("0.02", "0.010"),
        ("10000000000000000000000000000000000000000", "1"),
        ("1", "1.00"),
        ("0.01", "0.0101"),
        ("9", "10"),
        ("NaN", "1.0"),
        ("Infinity", "1"),
    ] {
        let row = admin.query_one(
            "SELECT $1::text::numeric::text, CASE WHEN $1::text::numeric > $2::text::numeric THEN ($1::text::numeric - $2::text::numeric)::text END",
            &[&quantity, &requested],
        ).await?;
        let loaded: String = row.get(0);
        let expected: Option<String> = row.get(1);
        let source = decision::Inventory {
            id: "source",
            product_id: "product",
            packaging_id: "packaging-a",
            location_id: "location-a",
            quantity: loaded,
            disposition: "held",
            lifecycle: "open",
        };
        let state = decision::State {
            source: &source,
            source_packaging: Some(decision::Packaging {
                id: "packaging-a",
                location_id: "location-a",
                lifecycle: "open",
            }),
            destination: Some(decision::Packaging {
                id: "packaging-b",
                location_id: "location-b",
                lifecycle: "open",
            }),
        };
        let command = decision::Command {
            new_inventory_id: "new",
            quantity: requested,
            to_packaging_id: "packaging-b",
            to_location_id: "location-b",
        };
        let actual = match decision::decide(state, command) {
            Ok(transition) => Some(transition.inventory[0].quantity.clone()),
            Err(refusal) => {
                anyhow::ensure!(
                    refusal.r#type == decision::RefusalType::InsufficientQuantity,
                    "numeric corpus reaches only quantity refusal: {refusal:?}"
                );
                None
            }
        };
        anyhow::ensure!(
            actual == expected,
            "split kernel differs from PostgreSQL for {quantity} minus {requested}: {actual:?} / {expected:?}"
        );
    }
    Ok(())
}
