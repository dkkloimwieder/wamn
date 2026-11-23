use anyhow::Context as _;
use serde_json::{Value, json};

use crate::business_fixture::RuntimePhase;

const OCCURRED_AT: &str = "2026-09-05T12:00:00.000000Z";

fn move_body(
    request_id: &str,
    idempotency_key: &str,
    runtime: &RuntimePhase,
    expected_revision: i64,
) -> Value {
    json!([{
        "request_id": request_id,
        "value": {
            "idempotency_key": idempotency_key,
            "packaging_id": runtime.packaging_id,
            "to_location_id": runtime.to_location_id,
            "expected_row_version": expected_revision,
            "occurred_at": OCCURRED_AT,
        }
    }])
}

/// The single request element of an array envelope, validated to carry the request id it
/// was asked with.
fn request_element<'a>(answer: &'a Value, request_id: &str) -> anyhow::Result<&'a Value> {
    let request_elements = answer
        .as_array()
        .context("the route answers with an array envelope")?;
    anyhow::ensure!(
        request_elements.len() == 1,
        "one request, one request element; got {}",
        request_elements.len()
    );
    anyhow::ensure!(
        request_elements[0]["request_id"] == request_id,
        "the request element answers request {request_id}: {}",
        request_elements[0]
    );
    Ok(&request_elements[0])
}
