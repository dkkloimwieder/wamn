//! Typed Acme purchase-order update boundary over its package-owned SQL.

use super::exports::client_acme_receiving::purchase_order::update as contract;
use super::{ErrorDetail, NodeError};
use wamn_client_acme_receiving_data_access::operation::{self, PurchaseOrderRow};
use wamn_client_acme_receiving_data_access::{AccessError, AccessErrorKind};

pub(super) mod codec {
    use super::contract;

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wit/purchase_order_update_codec.rs"
    ));
}

pub(super) async fn run(
    input: Vec<contract::UpdateItem>,
) -> Result<Vec<contract::UpdateOutcome>, NodeError> {
    if !(1..=100).contains(&input.len()) {
        return Err(invalid_input("operation input item count must be 1..=100"));
    }
    if input.iter().any(|item| item.request_id.is_empty()) {
        return Err(invalid_input(
            "every operation item must carry a nonempty string request_id",
        ));
    }

    let mut connection = wamn_postgres_statements::Connection::new();
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Err(error) => Err(contract::UpdateError::InvalidInput(error)),
            Ok(request) => operation::purchase_order_update(
                &mut connection,
                &request.id,
                request.expected_row_version,
                request.change.acme_inspection_required,
                request.change.acme_quality_status,
            )
            .await
            .map(result)
            .map_err(|error| access_error(&error, &request.id, request.expected_row_version)),
        };
        output.push(contract::UpdateOutcome {
            request_id: item.request_id,
            outcome,
        });
    }
    Ok(output)
}

fn result(row: PurchaseOrderRow) -> contract::UpdateResult {
    contract::UpdateResult {
        acme_inspection_required: row.acme_inspection_required,
        acme_quality_status: row.acme_quality_status,
        created_at: row.created_at.0,
        created_by: row.created_by.0,
        id: row.id.0,
        purchase_order_number: row.purchase_order_number,
        row_version: row.row_version,
        status: row.status,
        supplier_id: row.supplier_id.0,
        updated_at: row.updated_at.0,
        updated_by: row.updated_by.0,
    }
}

fn access_error(error: &AccessError, id: &str, expected: i64) -> contract::UpdateError {
    use contract::UpdateError;
    match error.kind() {
        AccessErrorKind::InvalidInput => {
            error.field().map_or(UpdateError::InternalError, |field| {
                UpdateError::InvalidInput(contract::InvalidInputDetail {
                    field: field.to_owned(),
                })
            })
        }
        AccessErrorKind::NotFound => UpdateError::NotFound(contract::NotFoundDetail {
            field: "id".to_owned(),
            id: id.to_owned(),
        }),
        AccessErrorKind::ConcurrencyConflict => {
            error
                .observed_row_version()
                .map_or(UpdateError::InternalError, |observed| {
                    UpdateError::ConcurrencyConflict(contract::ConcurrencyConflictDetail {
                        expected_row_version: expected.to_string(),
                        observed_row_version: observed.to_string(),
                    })
                })
        }
        AccessErrorKind::PermissionDenied => {
            UpdateError::PermissionDenied(contract::PermissionDeniedDetail {
                operation: "purchase_order.update".to_owned(),
            })
        }
        AccessErrorKind::Retry => UpdateError::Retry,
        AccessErrorKind::Timeout => UpdateError::Timeout,
        AccessErrorKind::ExclusionViolation | AccessErrorKind::InternalError => {
            UpdateError::InternalError
        }
    }
}

fn invalid_input(message: &str) -> NodeError {
    NodeError::InvalidInput(ErrorDetail {
        message: message.to_owned(),
        code: Some("invalid_input".to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::{codec, contract};

    fn decode(change: &str, revision: &str) -> contract::UpdateRequest {
        let input = format!(
            r#"[{{"request_id":"update-1","id":"00000000-0000-0000-0000-000000000001","expected_row_version":"{revision}","change":{change}}}]"#
        );
        codec::decode(&input).unwrap().pop().unwrap().input.unwrap()
    }

    #[test]
    fn update_codec_preserves_int64_and_change_states() {
        assert_eq!(decode("{}", "+01").expected_row_version, 1);
        assert_eq!(
            decode("{}", "9223372036854775807").expected_row_version,
            i64::MAX
        );
        let omitted = decode("{}", "1").change;
        assert_eq!(omitted.acme_inspection_required, None);
        assert_eq!(omitted.acme_quality_status, None);
        let changed = decode(
            r#"{"acme_inspection_required":true,"acme_quality_status":null}"#,
            "1",
        )
        .change;
        assert_eq!(changed.acme_inspection_required, Some(Some(true)));
        assert_eq!(changed.acme_quality_status, Some(None));

        let rejected = codec::decode(
            r#"[{"request_id":"wrong-owner","id":"00000000-0000-0000-0000-000000000001","expected_row_version":"1","change":{"supplier_id":"00000000-0000-0000-0000-000000000002"}}]"#,
        )
        .unwrap();
        let error = rejected[0].input.as_ref().unwrap_err();
        assert_eq!(error.field, "input");

        let mut input = rejected;
        input.push(contract::UpdateItem {
            request_id: "null".to_owned(),
            input: Ok(decode(r#"{"acme_quality_status":null}"#, "1")),
        });
        let mut call = std::pin::pin!(super::run(input));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let std::task::Poll::Ready(Ok(mut output)) =
            std::future::Future::poll(call.as_mut(), &mut context)
        else {
            panic!("invalid changes must refuse before database work");
        };
        output.push(contract::UpdateOutcome {
            request_id: "updated".to_owned(),
            outcome: Ok(contract::UpdateResult {
                id: "order-1".to_owned(),
                purchase_order_number: "PO-1".to_owned(),
                supplier_id: "supplier-1".to_owned(),
                status: "open".to_owned(),
                row_version: i64::MAX,
                created_at: "2026-09-19T12:00:00Z".to_owned(),
                created_by: "creator".to_owned(),
                updated_at: "2026-09-19T12:01:00Z".to_owned(),
                updated_by: "editor".to_owned(),
                acme_inspection_required: true,
                acme_quality_status: "pending".to_owned(),
            }),
        });
        let encoded: serde_json::Value = serde_json::from_str(&codec::encode(&output)).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!([
                {"request_id":"wrong-owner","error":{"code":"invalid_input","detail":{"field":"input"}}},
                {"request_id":"null","error":{"code":"invalid_input","detail":{"field":"change.acme_quality_status"}}},
                {"request_id":"updated","value":{
                    "id":"order-1","purchase_order_number":"PO-1","supplier_id":"supplier-1",
                    "status":"open","row_version":"9223372036854775807",
                    "created_at":"2026-09-19T12:00:00Z","created_by":"creator",
                    "updated_at":"2026-09-19T12:01:00Z","updated_by":"editor",
                    "acme_inspection_required":true,"acme_quality_status":"pending"
                }}
            ])
        );
    }
}
