//! Exact command SQL over the local business fixture, including lock waits.

use anyhow::Context as _;
use serde_json::{Value, json};
use tokio_postgres::{Client, SimpleQueryMessage, SimpleQueryRow};

const SOURCE: &str = "00000000-0000-0000-0000-000000000401";
const TARGET: &str = "00000000-0000-0000-0000-000000000402";
const PRODUCT: &str = "00000000-0000-0000-0000-000000000101";
const MISSING: &str = "00000000-0000-0000-0000-000000000499";
const LOCATION: &str = "00000000-0000-0000-0000-000000000201";
const OCCURRED: &str = "2026-09-05T00:00:00Z";
const CONTEXT: &str = "SET LOCAL search_path = wms, public;
    SELECT set_config('app.user_id', '00000000-0000-4000-8000-0000000000f1', true),
           set_config('app.operation', 'admin:inventory-sql-test', true);";

async fn run(client: &Client, sql: &str) -> anyhow::Result<SimpleQueryRow> {
    client
        .simple_query(sql)
        .await?
        .into_iter()
        .find_map(|message| match message {
            SimpleQueryMessage::Row(row) => Some(row),
            _ => None,
        })
        .context("the command returns an outcome row")
}

fn outcome(row: &SimpleQueryRow, expected: &str, detail: &Value) {
    assert_eq!(row.get("outcome"), Some(expected));
    assert_eq!(
        &serde_json::from_str::<Value>(row.get("detail").unwrap()).unwrap(),
        detail
    );
}

async fn prepare(client: &Client) -> anyhow::Result<()> {
    client
        .batch_execute(&format!(
            "SET search_path = wms, public;
         PREPARE test_lock_adjust AS {} PREPARE test_adjust AS {}
         PREPARE test_lock_split AS {} PREPARE test_split AS {}
         PREPARE test_lock_merge AS {} PREPARE test_merge AS {}",
            include_str!("../command/inventory_adjust/lock_packaging.sql"),
            include_str!("../command/inventory_adjust/adjust.sql"),
            include_str!("../command/inventory_split/lock_packaging.sql"),
            include_str!("../command/inventory_split/split.sql"),
            include_str!("../command/inventory_merge/lock_both_packagings.sql"),
            include_str!("../command/inventory_merge/merge.sql"),
        ))
        .await?;
    Ok(())
}

async fn seed(client: &Client) -> anyhow::Result<()> {
    client
        .batch_execute(&format!(
            "INSERT INTO wms.packaging (id, packaging_code, type, location_id, status)
         VALUES ('{SOURCE}', 'SQL-SOURCE', 'pallet', '{LOCATION}', 'held'),
                ('{TARGET}', 'SQL-TARGET', 'tote', '{LOCATION}', 'available');
         INSERT INTO wms.packaging_quantity (packaging_id, product_id, status, quantity)
         VALUES ('{SOURCE}', '{PRODUCT}', 'available', 10),
                ('{SOURCE}', '{PRODUCT}', 'held', 2),
                ('{TARGET}', '{PRODUCT}', 'available', 4);"
        ))
        .await?;
    Ok(())
}

/// Exercise refusal precedence, ordered merge results, and dependent writes.
pub(crate) async fn assert_outcomes(client: &Client) -> anyhow::Result<()> {
    prepare(client).await?;
    client.batch_execute(&format!("BEGIN; {CONTEXT}")).await?;
    seed(client).await?;
    for (command, field) in [("adjust", "packaging_id"), ("split", "source_packaging_id")] {
        let row = run(
            client,
            &format!("EXECUTE test_lock_{command}('{MISSING}', 0)"),
        )
        .await?;
        outcome(
            &row,
            "packaging_not_found",
            &json!({"field":format!("value.{field}"), "id":MISSING}),
        );
        let row = run(
            client,
            &format!("EXECUTE test_lock_{command}('{SOURCE}', 0)"),
        )
        .await?;
        outcome(
            &row,
            "concurrency_conflict",
            &json!({"expected_row_version":0, "observed_row_version":1}),
        );
    }
    let row = run(
        client,
        &format!("EXECUTE test_lock_merge('{MISSING}', '{MISSING}', 0)"),
    )
    .await?;
    outcome(
        &row,
        "packaging_not_found",
        &json!({"field":"value.source_packaging_id", "id":MISSING}),
    );
    let row = run(
        client,
        &format!("EXECUTE test_lock_merge('{SOURCE}', '{MISSING}', 0)"),
    )
    .await?;
    outcome(
        &row,
        "packaging_not_found",
        &json!({"field":"value.target_packaging_id", "id":MISSING}),
    );
    let row = run(
        client,
        &format!("EXECUTE test_lock_merge('{SOURCE}', '{TARGET}', 0)"),
    )
    .await?;
    outcome(
        &row,
        "concurrency_conflict",
        &json!({"expected_row_version":0, "observed_row_version":1}),
    );
    let row = run(client, &format!("EXECUTE test_lock_split('{SOURCE}', 1)")).await?;
    outcome(&row, "ok", &json!({}));
    let split = |product, quantity, location| {
        format!(
            "EXECUTE test_split('{SOURCE}', '{product}', 'available', {quantity}, 'SQL-SPLIT', 'case', '{location}', '{OCCURRED}')"
        )
    };
    let row = run(client, &split(MISSING, 100, MISSING)).await?;
    outcome(
        &row,
        "location_not_found",
        &json!({"field":"value.to_location_id", "id":MISSING}),
    );
    let row = run(client, &split(MISSING, 100, LOCATION)).await?;
    outcome(
        &row,
        "quantity_not_found",
        &json!({"field":"value.product_id", "id":MISSING}),
    );
    let row = run(client, &split(PRODUCT, 10, LOCATION)).await?;
    outcome(
        &row,
        "insufficient_quantity",
        &json!({"field":"value.quantity", "observed":"10"}),
    );
    let adjust = |product, quantity| {
        format!(
            "EXECUTE test_adjust('{SOURCE}', '{product}', 'available', {quantity}, 'count', '{OCCURRED}')"
        )
    };
    let row = run(client, &adjust(MISSING, 10)).await?;
    outcome(
        &row,
        "quantity_not_found",
        &json!({"field":"value.product_id", "id":MISSING}),
    );
    let row = run(client, &adjust(PRODUCT, 10)).await?;
    outcome(&row, "invalid_input", &json!({"field":"value.quantity"}));
    // Refusals leave all writes untouched.
    let row = run(client, &format!("SELECT row_version::text, (SELECT count(*) FROM wms.inventory_transaction WHERE from_packaging_id = '{SOURCE}' OR to_packaging_id = '{SOURCE}')::text AS transactions FROM wms.packaging WHERE id = '{SOURCE}'")).await?;
    assert_eq!(row.get("row_version"), Some("1"));
    assert_eq!(row.get("transactions"), Some("0"));

    let row = run(client, &split(PRODUCT, 3, LOCATION)).await?;
    outcome(&row, "ok", &json!({}));
    assert_eq!(row.get("status"), Some("held"));
    assert_eq!(row.get("row_version"), Some("2"));
    let new_id = row
        .get("new_packaging_id")
        .context("split creates a packaging")?;
    let row = run(client, &format!("SELECT packaging.status, quantity.quantity::text FROM wms.packaging AS packaging JOIN wms.packaging_quantity AS quantity ON quantity.packaging_id = packaging.id WHERE packaging.id = '{new_id}'")).await?;
    assert_eq!(row.get("status"), Some("held"));
    assert_eq!(row.get("quantity"), Some("3"));
    let row = run(client, &adjust(PRODUCT, 12)).await?;
    outcome(&row, "ok", &json!({}));
    let id = row
        .get("transaction_id")
        .context("adjust returns a transaction")?;
    let row = run(client, &format!("SELECT quantity::text, from_packaging_id::text, to_packaging_id::text FROM wms.inventory_transaction WHERE id = '{id}'")).await?;
    assert_eq!(row.get("quantity"), Some("5"));
    assert_eq!(row.get("from_packaging_id"), None);
    assert_eq!(row.get("to_packaging_id"), Some(SOURCE));

    let row = run(
        client,
        &format!("EXECUTE test_lock_merge('{SOURCE}', '{TARGET}', 1)"),
    )
    .await?;
    outcome(&row, "ok", &json!({}));
    let row = run(
        client,
        &format!("EXECUTE test_merge('{SOURCE}', '{TARGET}', '{OCCURRED}')"),
    )
    .await?;
    outcome(&row, "ok", &json!({}));
    let ids: Vec<String> = serde_json::from_str(row.get("transaction_ids").unwrap())?;
    assert_eq!(ids.len(), 2);
    for (id, status, quantity) in [(&ids[0], "available", "12"), (&ids[1], "held", "2")] {
        let row = run(client, &format!("SELECT from_status, quantity::text FROM wms.inventory_transaction WHERE id = '{id}'")).await?;
        assert_eq!(row.get("from_status"), Some(status));
        assert_eq!(row.get("quantity"), Some(quantity));
    }
    let row = run(client, &format!("SELECT status, (SELECT count(*) FROM wms.packaging_quantity WHERE packaging_id = '{SOURCE}')::text AS balances FROM wms.packaging WHERE id = '{SOURCE}'")).await?;
    assert_eq!(row.get("status"), Some("consumed"));
    assert_eq!(row.get("balances"), Some("0"));
    for command in ["adjust", "split"] {
        let row = run(
            client,
            &format!("EXECUTE test_lock_{command}('{SOURCE}', 0)"),
        )
        .await?;
        let field = if command == "adjust" {
            "packaging_id"
        } else {
            "source_packaging_id"
        };
        outcome(
            &row,
            "packaging_not_found",
            &json!({"field":format!("value.{field}"), "id":SOURCE}),
        );
    }
    let row = run(
        client,
        &format!("EXECUTE test_lock_merge('{SOURCE}', '{TARGET}', 0)"),
    )
    .await?;
    outcome(
        &row,
        "packaging_not_found",
        &json!({"field":"value.source_packaging_id", "id":SOURCE}),
    );
    let row = run(
        client,
        &format!("EXECUTE test_merge('{SOURCE}', '{TARGET}', '{OCCURRED}')"),
    )
    .await?;
    outcome(
        &row,
        "invalid_input",
        &json!({"field":"value.source_packaging_id"}),
    );
    let row = run(client, &format!("SELECT quantity::text FROM wms.packaging_quantity WHERE packaging_id = '{TARGET}' AND status = 'available'")).await?;
    assert_eq!(row.get("quantity"), Some("16"));
    client.batch_execute("ROLLBACK").await?;
    Ok(())
}

/// Wait for an actual parent-lock contention, then test the fresh child read.
pub(crate) async fn assert_fresh_reads(client: &Client, url: &str) -> anyhow::Result<()> {
    let (waiter, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls).await?;
    let connection = tokio::spawn(connection);
    prepare(&waiter).await?;
    let pid: i32 = waiter
        .query_one("SELECT pg_backend_pid()", &[])
        .await?
        .get(0);
    for command in ["adjust", "split", "merge"] {
        client.batch_execute(&format!("BEGIN; {CONTEXT}")).await?;
        seed(client).await?;
        client.batch_execute("COMMIT").await?;
        client.batch_execute(&format!("BEGIN; {CONTEXT} SELECT id FROM wms.packaging WHERE id = '{SOURCE}' FOR UPDATE;
            UPDATE wms.packaging_quantity SET quantity = 6 WHERE packaging_id = '{SOURCE}' AND status = 'available';
            INSERT INTO wms.packaging_quantity (packaging_id, product_id, status, quantity) VALUES ('{TARGET}', '{PRODUCT}', 'held', 3);
            UPDATE wms.packaging SET row_version = 2 WHERE id = '{SOURCE}';")).await?;
        waiter.batch_execute(&format!("BEGIN; {CONTEXT}")).await?;
        let lock = if command == "merge" {
            format!("EXECUTE test_lock_merge('{SOURCE}', '{TARGET}', 1)")
        } else {
            format!("EXECUTE test_lock_{command}('{SOURCE}', 2)")
        };
        let waiting = async {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let row = client
                        .query_one("SELECT cardinality(pg_blocking_pids($1)) > 0", &[&pid])
                        .await?;
                    if row.get::<_, bool>(0) {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                anyhow::Ok(())
            })
            .await
            .context("the second connection waits for the parent lock")??;
            client.batch_execute("COMMIT").await?;
            anyhow::Ok(())
        };
        let (locked, ()) = tokio::try_join!(run(&waiter, &lock), waiting)?;
        outcome(&locked, "ok", &json!({}));
        let sql = match command {
            "adjust" => format!(
                "EXECUTE test_adjust('{SOURCE}', '{PRODUCT}', 'available', 6, 'count', '{OCCURRED}')"
            ),
            "split" => format!(
                "EXECUTE test_split('{SOURCE}', '{PRODUCT}', 'available', 6, 'SQL-SPLIT', 'case', '{LOCATION}', '{OCCURRED}')"
            ),
            _ => format!("EXECUTE test_merge('{SOURCE}', '{TARGET}', '{OCCURRED}')"),
        };
        let row = run(&waiter, &sql).await?;
        match command {
            "adjust" => outcome(&row, "invalid_input", &json!({"field":"value.quantity"})),
            "split" => outcome(
                &row,
                "insufficient_quantity",
                &json!({"field":"value.quantity", "observed":"6"}),
            ),
            _ => {
                outcome(&row, "ok", &json!({}));
                let row = run(&waiter, &format!("SELECT quantity::text FROM wms.packaging_quantity WHERE packaging_id = '{TARGET}' AND status = 'held'")).await?;
                assert_eq!(row.get("quantity"), Some("5"));
                let row = run(&waiter, &format!("SELECT quantity::text FROM wms.inventory_transaction WHERE from_packaging_id = '{SOURCE}' AND from_status = 'available'")).await?;
                assert_eq!(row.get("quantity"), Some("6"));
            }
        }
        waiter.batch_execute("ROLLBACK").await?;
        client
            .batch_execute(&format!(
                "BEGIN; {CONTEXT}
            DELETE FROM wms.packaging_quantity WHERE packaging_id IN ('{SOURCE}', '{TARGET}');
            DELETE FROM wms.packaging WHERE id IN ('{SOURCE}', '{TARGET}'); COMMIT;"
            ))
            .await?;
    }
    drop(waiter);
    connection.abort();
    Ok(())
}
