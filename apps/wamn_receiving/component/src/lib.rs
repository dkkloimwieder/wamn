#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting every Receiving operation.

use exports::wamn_receiving::location::list::Guest as LocationList;
use exports::wamn_receiving::purchase_order::get::Guest as PurchaseOrderGet;
use exports::wamn_receiving::purchase_order::query::Guest as PurchaseOrderQuery;
use exports::wamn_receiving::purchase_order::update::Guest as PurchaseOrderUpdate;
use exports::wamn_receiving::receipt::get::Guest as ReceiptGet;
use exports::wamn_receiving::receipt::query::Guest as ReceiptQuery;
use exports::wamn_receiving::receiving::load_purchase_order_history::Guest as LoadPurchaseOrderHistory;
use exports::wamn_receiving::receiving::load_receipt_screen::Guest as LoadReceiptScreen;
use exports::wamn_receiving::receiving::record_receipt::{
    self as contract, Guest as RecordReceipt,
};
use wamn::node::types::{Emission, NodeContext, NodeError};
use wamn_receiving_data_access::record_receipt as receipt;

wit_bindgen::generate!({
    world: "wamn:receiving-component/receiving@0.1.0",
    inline: r#"
        package wamn:receiving-component@0.1.0;

        world receiving {
          import wamn:postgres/types@0.1.0;
          import wamn:postgres/statements@0.1.0;
          export wamn-receiving:location/%list@1.0.0;
          export wamn-receiving:purchase-order/get@1.0.0;
          export wamn-receiving:purchase-order/query@1.0.0;
          export wamn-receiving:purchase-order/update@1.0.0;
          export wamn-receiving:receipt/get@1.0.0;
          export wamn-receiving:receipt/query@1.0.0;
          export wamn-receiving:receiving/load-purchase-order-history@1.0.0;
          export wamn-receiving:receiving/load-receipt-screen@1.0.0;
          export wamn-receiving:receiving/record-receipt@1.0.0;
        }
    "#,
    path: [
        "../data/wit/deps/wamn-node",
        "../data/wit/deps/wamn-postgres",
        "../data/wit/deps/wamn-receiving-location",
        "../data/wit/deps/wamn-receiving-purchase-order",
        "../data/wit/deps/wamn-receiving-receipt",
        "../generated/wit/deps/wamn-receiving-receiving",
    ],
    generate_all,
    async: true,
});

struct Component;

mod receipt_codec {
    use super::contract;

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wit/receiving_record_receipt_codec.rs"
    ));
}

async fn invoke_operation<F>(operation: F) -> Result<Emission, NodeError>
where
    F: Future<Output = Result<String, wamn_receiving_data_access::operation::InvocationError>>,
{
    operation
        .await
        .map(|payload| Emission {
            payload,
            port: None,
        })
        .map_err(|error| {
            NodeError::InvalidInput(wamn::node::types::ErrorDetail {
                message: error.context().to_owned(),
                code: Some(error.code().to_owned()),
            })
        })
}

impl LocationList for Component {
    async fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(wamn_receiving_data_access::operation::location_list(&input)).await
    }
}

impl PurchaseOrderGet for Component {
    async fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(wamn_receiving_data_access::operation::purchase_order_get(
            &input,
        ))
        .await
    }
}

impl PurchaseOrderQuery for Component {
    async fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(wamn_receiving_data_access::operation::purchase_order_query(
            &input,
        ))
        .await
    }
}

impl PurchaseOrderUpdate for Component {
    async fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(wamn_receiving_data_access::operation::purchase_order_update(&input)).await
    }
}

impl ReceiptGet for Component {
    async fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(wamn_receiving_data_access::operation::receipt_get(&input)).await
    }
}

impl ReceiptQuery for Component {
    async fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(wamn_receiving_data_access::operation::receipt_query(&input)).await
    }
}

impl LoadReceiptScreen for Component {
    async fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(
            wamn_receiving_data_access::operation::receiving_load_receipt_screen(&input),
        )
        .await
    }
}

impl LoadPurchaseOrderHistory for Component {
    async fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(
            wamn_receiving_data_access::operation::receiving_load_purchase_order_history(&input),
        )
        .await
    }
}

impl RecordReceipt for Component {
    async fn run(
        _context: NodeContext,
        input: Vec<contract::RecordReceiptItem>,
    ) -> Result<Vec<contract::RecordReceiptOutcome>, NodeError> {
        record_receipt(input).await
    }

    async fn run_json(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let input =
            receipt_codec::decode(&input).map_err(|error| invalid_input(error.context()))?;
        let output = record_receipt(input).await?;
        Ok(Emission {
            payload: receipt_codec::encode(&output),
            port: None,
        })
    }
}

async fn record_receipt(
    input: Vec<contract::RecordReceiptItem>,
) -> Result<Vec<contract::RecordReceiptOutcome>, NodeError> {
    if !(1..=receipt::MAX_RECORD_RECEIPT_ITEMS).contains(&input.len()) {
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
        let request_id = item.request_id;
        let request = match item.input {
            Ok(request) => request,
            Err(error) => {
                output.push(contract::RecordReceiptOutcome {
                    request_id,
                    outcome: Err(error),
                });
                continue;
            }
        };
        let command = receipt::RecordReceiptInput {
            request_id: request_id.clone().into_boxed_str(),
            value: receipt::RecordReceiptValue {
                idempotency_key: request.idempotency_key.into_boxed_str(),
                purchase_order_id: request.purchase_order_id.into_boxed_str(),
                receipt_reference: request.receipt_reference.into_boxed_str(),
                occurred_at: request.occurred_at.into_boxed_str(),
                line: request
                    .line
                    .into_iter()
                    .map(|line| receipt::RecordReceiptLine {
                        purchase_order_line_id: line.purchase_order_line_id.into_boxed_str(),
                        quantity: line.quantity.into_boxed_str(),
                        location_id: line.location_id.into_boxed_str(),
                    })
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            },
        };
        let outcome = match receipt::record_receipt(&mut connection, &[command]).await {
            Ok(outcomes) => outcomes
                .into_vec()
                .pop()
                .expect("one command yields one correlated result"),
            Err(error) => receipt::RecordReceiptItemOutcome::Refused {
                request_id: request_id.clone().into_boxed_str(),
                error,
            },
        };
        output.push(record_receipt_outcome(outcome));
    }
    Ok(output)
}

fn invalid_input(message: &str) -> NodeError {
    NodeError::InvalidInput(wamn::node::types::ErrorDetail {
        message: message.to_owned(),
        code: Some("invalid_input".to_owned()),
    })
}

fn record_receipt_outcome(
    value: receipt::RecordReceiptItemOutcome,
) -> contract::RecordReceiptOutcome {
    match value {
        receipt::RecordReceiptItemOutcome::Succeeded { request_id, value } => {
            contract::RecordReceiptOutcome {
                request_id: request_id.into(),
                outcome: Ok(contract::RecordReceiptResult {
                    receipt_id: value.receipt_id.into(),
                    purchase_order_id: value.purchase_order_id.into(),
                    purchase_order_status: value.purchase_order_status.as_str().to_owned(),
                    row_version: value.row_version,
                }),
            }
        }
        receipt::RecordReceiptItemOutcome::Refused { request_id, error } => {
            contract::RecordReceiptOutcome {
                request_id: request_id.into(),
                outcome: Err(record_receipt_error(&error)),
            }
        }
    }
}

fn record_receipt_error(error: &receipt::RecordReceiptError) -> contract::RecordReceiptError {
    let field = || error.field().map(str::to_owned);
    let field_id = || {
        error
            .field()
            .zip(error.id())
            .map(|(field, id)| (field.to_owned(), id.to_owned()))
    };
    match error.kind() {
        receipt::RecordReceiptErrorKind::InvalidInput => match field() {
            Some(field) => {
                contract::RecordReceiptError::InvalidInput(contract::InvalidInputDetail {
                    field,
                    minimum: error.minimum().and_then(|value| i64::try_from(value).ok()),
                    maximum: error.maximum().and_then(|value| i64::try_from(value).ok()),
                    observed: error.observed().and_then(|value| i64::try_from(value).ok()),
                })
            }
            None => contract::RecordReceiptError::InternalError,
        },
        receipt::RecordReceiptErrorKind::PurchaseOrderNotFound => {
            field().map_or(contract::RecordReceiptError::InternalError, |field| {
                contract::RecordReceiptError::PurchaseOrderNotFound(
                    contract::PurchaseOrderNotFoundDetail { field },
                )
            })
        }
        receipt::RecordReceiptErrorKind::PurchaseOrderNotOpen => {
            field().map_or(contract::RecordReceiptError::InternalError, |field| {
                contract::RecordReceiptError::PurchaseOrderNotOpen(
                    contract::PurchaseOrderNotOpenDetail { field },
                )
            })
        }
        receipt::RecordReceiptErrorKind::PurchaseOrderLineNotFound => field_id().map_or(
            contract::RecordReceiptError::InternalError,
            |(field, id)| {
                contract::RecordReceiptError::PurchaseOrderLineNotFound(
                    contract::PurchaseOrderLineNotFoundDetail { field, id },
                )
            },
        ),
        receipt::RecordReceiptErrorKind::PurchaseOrderLineMismatch => field_id().map_or(
            contract::RecordReceiptError::InternalError,
            |(field, id)| {
                contract::RecordReceiptError::PurchaseOrderLineMismatch(
                    contract::PurchaseOrderLineMismatchDetail { field, id },
                )
            },
        ),
        receipt::RecordReceiptErrorKind::LocationNotFound => field_id().map_or(
            contract::RecordReceiptError::InternalError,
            |(field, id)| {
                contract::RecordReceiptError::LocationNotFound(contract::LocationNotFoundDetail {
                    field,
                    id,
                })
            },
        ),
        receipt::RecordReceiptErrorKind::QuantityExceedsRemaining => field_id().map_or(
            contract::RecordReceiptError::InternalError,
            |(field, id)| {
                contract::RecordReceiptError::QuantityExceedsRemaining(
                    contract::QuantityExceedsRemainingDetail { field, id },
                )
            },
        ),
        receipt::RecordReceiptErrorKind::ReceiptReferenceConflict => {
            error
                .constraint()
                .map_or(contract::RecordReceiptError::InternalError, |constraint| {
                    contract::RecordReceiptError::ReceiptReferenceConflict(
                        contract::ReceiptReferenceConflictDetail {
                            constraint: constraint.to_owned(),
                        },
                    )
                })
        }
        receipt::RecordReceiptErrorKind::IdempotencyConflict => {
            field().map_or(contract::RecordReceiptError::InternalError, |field| {
                contract::RecordReceiptError::IdempotencyConflict(
                    contract::IdempotencyConflictDetail { field },
                )
            })
        }
        receipt::RecordReceiptErrorKind::Retry => contract::RecordReceiptError::Retry,
        receipt::RecordReceiptErrorKind::Timeout => contract::RecordReceiptError::Timeout,
        receipt::RecordReceiptErrorKind::PermissionDenied => {
            contract::RecordReceiptError::PermissionDenied(contract::PermissionDeniedDetail {
                operation: "receiving.record_receipt".to_owned(),
            })
        }
        receipt::RecordReceiptErrorKind::InternalError => {
            contract::RecordReceiptError::InternalError
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{contract, receipt_codec};

    #[test]
    fn typed_call_refuses_missing_correlation_before_database_work() {
        let mut input = receipt_codec::decode(&format!(
            r#"[{{"request_id":"first","value":{VALID_VALUE}}}]"#
        ))
        .unwrap();
        input.push(contract::RecordReceiptItem {
            request_id: String::new(),
            input: Err(contract::RecordReceiptError::InternalError),
        });
        let mut call = std::pin::pin!(super::record_receipt(input));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(matches!(
            std::future::Future::poll(call.as_mut(), &mut context),
            std::task::Poll::Ready(Err(super::NodeError::InvalidInput(_)))
        ));
    }

    const VALID_VALUE: &str = r#"{
        "idempotency_key":"key-1",
        "purchase_order_id":"00000000-0000-0000-0000-000000000001",
        "receipt_reference":"receipt-1",
        "occurred_at":"2026-09-19T12:34:56.000000Z",
        "line":[{
            "purchase_order_line_id":"00000000-0000-0000-0000-000000000002",
            "quantity":"12.3400",
            "location_id":"00000000-0000-0000-0000-000000000003"
        }]
    }"#;

    #[test]
    fn codec_preserves_correlation_and_exact_numeric_text() {
        let input = format!(
            r#"[{{"request_id":"valid","value":{VALID_VALUE}}},{{"request_id":"bad","value":{{"unknown":true}}}}]"#
        );
        let decoded = receipt_codec::decode(&input).expect("the envelope decodes");
        assert_eq!(decoded[0].request_id, "valid");
        let request = decoded[0].input.as_ref().expect("the valid item decodes");
        assert_eq!(request.line[0].quantity, "12.3400");
        assert_eq!(decoded[1].request_id, "bad");
        assert!(matches!(
            decoded[1].input,
            Err(contract::RecordReceiptError::InvalidInput(_))
        ));
    }

    #[test]
    fn codec_refuses_unknown_null_and_missing_fields_per_item() {
        let input = r#"[
            {"request_id":"unknown","value":{"unknown":true}},
            {"request_id":"null","value":null},
            {"request_id":"missing"}
        ]"#;
        let decoded = receipt_codec::decode(input).expect("the envelope decodes");
        assert_eq!(
            decoded
                .iter()
                .map(|item| item.request_id.as_str())
                .collect::<Vec<_>>(),
            ["unknown", "null", "missing"]
        );
        assert!(decoded.iter().all(|item| matches!(
            item.input,
            Err(contract::RecordReceiptError::InvalidInput(_))
        )));
    }

    #[test]
    fn codec_encodes_row_version_as_a_decimal_string() {
        let output = [contract::RecordReceiptOutcome {
            request_id: "request-1".to_owned(),
            outcome: Ok(contract::RecordReceiptResult {
                receipt_id: "00000000-0000-0000-0000-000000000001".to_owned(),
                purchase_order_id: "00000000-0000-0000-0000-000000000002".to_owned(),
                purchase_order_status: "open".to_owned(),
                row_version: 4_294_967_297,
            }),
        }];
        let encoded: serde_json::Value =
            serde_json::from_str(&receipt_codec::encode(&output)).expect("the output is JSON");
        assert_eq!(encoded[0]["request_id"], "request-1");
        assert_eq!(encoded[0]["value"]["row_version"], "4294967297");
    }
}

export!(Component);
