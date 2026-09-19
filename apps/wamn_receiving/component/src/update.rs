//! Typed purchase-order update boundary over the existing application operation.

use super::exports::wamn_receiving::purchase_order::update as contract;
use super::{NodeError, invalid_input};
use wamn_receiving_data_access::{AccessError, AccessErrorKind, purchase_order};

pub(super) mod codec {
    use super::contract;
    include!("../../generated/wit/purchase_order_update_codec.rs");
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
            Ok(request) => {
                let supplier_id = match request.change.supplier_id {
                    None => purchase_order::SupplierIdUpdate::Omitted,
                    Some(None) => purchase_order::SupplierIdUpdate::Null,
                    Some(Some(value)) => purchase_order::SupplierIdUpdate::Value(value.into()),
                };
                purchase_order::update(
                    &mut connection,
                    &request.id,
                    request.expected_row_version,
                    supplier_id,
                )
                .await
                .map(result)
                .map_err(|error| access_error(&error, &request.id, request.expected_row_version))
            }
        };
        output.push(contract::UpdateOutcome {
            request_id: item.request_id,
            outcome,
        });
    }
    Ok(output)
}

fn result(row: purchase_order::PurchaseOrderRow) -> contract::UpdateResult {
    contract::UpdateResult {
        id: row.id.0,
        purchase_order_number: row.purchase_order_number,
        supplier_id: row.supplier_id.0,
        status: row.status,
        row_version: row.row_version,
        created_at: row.created_at.0,
        created_by: row.created_by.0,
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
        AccessErrorKind::UniqueViolation
        | AccessErrorKind::ForeignKeyViolation
        | AccessErrorKind::CheckViolation
        | AccessErrorKind::ExclusionViolation
        | AccessErrorKind::InternalError => UpdateError::InternalError,
    }
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
    fn two_spellings_of_one_int64_make_one_command() {
        assert_eq!(
            decode("{}", "01").expected_row_version,
            decode("{}", "1").expected_row_version
        );
        assert_eq!(decode("{}", "+1").expected_row_version, 1);
        assert_eq!(
            decode("{}", "9223372036854775807").expected_row_version,
            i64::MAX
        );
        let output = [contract::UpdateOutcome {
            request_id: "conflict".to_owned(),
            outcome: Err(contract::UpdateError::ConcurrencyConflict(
                contract::ConcurrencyConflictDetail {
                    expected_row_version: i64::MAX.to_string(),
                    observed_row_version: "4294967297".to_owned(),
                },
            )),
        }];
        let encoded: serde_json::Value = serde_json::from_str(&codec::encode(&output)).unwrap();
        assert_eq!(encoded[0]["request_id"], "conflict");
        assert_eq!(
            encoded[0]["error"]["detail"]["expected_row_version"],
            "9223372036854775807"
        );
        assert_eq!(
            encoded[0]["error"]["detail"]["observed_row_version"],
            "4294967297"
        );
    }

    #[test]
    fn update_preserves_omitted_null_and_value_states() {
        assert_eq!(decode("{}", "1").change.supplier_id, None);
        assert_eq!(
            decode(r#"{"supplier_id":null}"#, "1").change.supplier_id,
            Some(None)
        );
        assert_eq!(
            decode(
                r#"{"supplier_id":"00000000-0000-0000-0000-000000000002"}"#,
                "1"
            )
            .change
            .supplier_id,
            Some(Some("00000000-0000-0000-0000-000000000002".to_owned()))
        );
        let input = codec::decode(r#"[
            {"request_id":"null","id":"00000000-0000-0000-0000-000000000001","expected_row_version":"1","change":{"supplier_id":null}},
            {"request_id":"unknown","id":"00000000-0000-0000-0000-000000000001","expected_row_version":"1","change":{"status":"complete"}},
            {"request_id":"wrong-type","id":"00000000-0000-0000-0000-000000000001","expected_row_version":1,"change":{}}
        ]"#).unwrap();
        let mut call = std::pin::pin!(super::run(input));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let std::task::Poll::Ready(Ok(output)) =
            std::future::Future::poll(call.as_mut(), &mut context)
        else {
            panic!("invalid updates must refuse before database work");
        };
        let encoded: serde_json::Value = serde_json::from_str(&codec::encode(&output)).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!([
                {"request_id":"null","error":{"code":"invalid_input","detail":{"field":"change.supplier_id"}}},
                {"request_id":"unknown","error":{"code":"invalid_input","detail":{"field":"input"}}},
                {"request_id":"wrong-type","error":{"code":"invalid_input","detail":{"field":"input"}}}
            ])
        );
    }
}
