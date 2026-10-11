//! The exact HTTP refusal of a denied operation.

use super::*;

pub(super) fn assert_operation_refusal(
    response: &hyper::Response<Bytes>,
    code: &str,
    operation: &str,
) -> anyhow::Result<()> {
    let body: Value = serde_json::from_slice(response.body())?;
    anyhow::ensure!(
        response.status() == StatusCode::FORBIDDEN
            && body == serde_json::json!({"error": {"code": code, "operation": operation}}),
        "operation refusal differs from its exact HTTP contract: status={} body={body}",
        response.status()
    );
    Ok(())
}
