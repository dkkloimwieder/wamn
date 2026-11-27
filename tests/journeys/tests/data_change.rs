//! A Receiving receipt reaches the Acme overlay's Data change handler after an
//! oversized event in the same project. The handler creates the receipt's inspection.

mod common;

use std::time::Duration;

use anyhow::{Context as _, ensure};
use common::request;
use common::{App, Routes};
use serde_json::{Value, json};
use tokio::time::Instant;

/// How long CDC, the event stream and the handler take to commit the inspection.
const HANDLER_DEADLINE: Duration = Duration::from_secs(120);

/// Create one row through a generated create route, and return its answer value.
async fn create(routes: &Routes, model: &str, fields: Value) -> anyhow::Result<Value> {
    let id = uuid::Uuid::now_v7().to_string();
    let mut element = json!({"request_id": id, "idempotency_key": id, "id": id});
    element
        .as_object_mut()
        .context("request element")?
        .extend(fields.as_object().context("create fields")?.clone());
    let answer = routes.post(&format!("/{model}/create"), element).await?;
    answer
        .get("value")
        .cloned()
        .with_context(|| format!("{model}.create refused: {answer}"))
}

common::journey! {
    /// A goods-in receipt on a purchase order that requires inspection makes the
    /// `client_acme_receiving` Data change handler create a pending inspection.
    async fn a_receipt_creates_its_inspection() -> anyhow::Result<()> {
        let app = App::prepare(
            &request::fixture(request::ACME_RECEIVING_PACKAGE),
            request::ACME_RECEIVING_APP,
        ).await?;
        let routes = &app.routes;
        let suffix = uuid::Uuid::new_v4().simple().to_string();

        // The create request fits the HTTP body limit. The supplier's CDC record
        // has more metadata and exceeds the NATS limit, so capture must trim it
        // before the later receipt can reach its handler.
        let supplier_name = "s".repeat(1024 * 1024 - 200);
        let supplier = create(routes, "supplier", json!({"name": supplier_name})).await?;
        let item = create(
            routes,
            "item",
            json!({"item_number": format!("I-{suffix}")}),
        )
        .await?;
        let location = create(
            routes,
            "location",
            json!({"location_code": format!("L-{suffix}")}),
        )
        .await?;
        let order = create(
            routes,
            "purchase_order",
            json!({"supplier_id": supplier["id"]}),
        )
        .await?;
        let line = create(
            routes,
            "purchase_order_line",
            json!({"purchase_order_id": order["id"], "line_number": 1, "item_id": item["id"], "ordered_quantity": "5"}),
        )
        .await?;
        let required = routes
            .post(
                "/acme/purchase_order/update",
                json!({
                    "request_id": "require",
                    "id": order["id"],
                    "expected_row_version": order["row_version"],
                    "change": {
                        "acme_inspection_required": true,
                        "acme_quality_status": "pending",
                    },
                }),
            )
            .await?;
        ensure!(
            required.get("value").is_some(),
            "the order requires inspection: {required}"
        );

        let receipt = routes
            .post(
                "/receiving/record_receipt",
                json!({"request_id": "receipt", "value": {
                    "idempotency_key": "receipt",
                    "purchase_order_id": order["id"],
                    "receipt_reference": format!("R-{suffix}"),
                    "occurred_at": "2026-11-21T10:00:00Z",
                    "line": [{"purchase_order_line_id": line["id"], "location_id": location["id"], "quantity": "5"}],
                }}),
            )
            .await?;
        let receipt_id = receipt["value"]["receipt_id"]
            .as_str()
            .with_context(|| format!("the receipt records: {receipt}"))?
            .to_owned();

        let mut database = app.env.observe().await?;
        let end = Instant::now() + HANDLER_DEADLINE;
        loop {
            let status: Option<String> = sqlx::query_scalar(
                "SELECT status FROM receiving.quality_inspection WHERE receipt_id = $1::uuid",
            )
            .bind(&receipt_id)
            .fetch_optional(&mut database.connection)
            .await?;
            if let Some(status) = status {
                ensure!(
                    status == "pending",
                    "the handler's inspection is pending, not {status}"
                );
                return Ok(());
            }
            ensure!(
                Instant::now() < end,
                "receipt {receipt_id} has no inspection within {HANDLER_DEADLINE:?}"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}
