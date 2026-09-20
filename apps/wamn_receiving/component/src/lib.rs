#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting every Receiving operation.

use exports::wamn_receiving::receiving::record_receipt as contract;
use wamn::node::types::NodeError;
use wamn_receiving_data_access::record_receipt as receipt;

mod reads;
mod update;

wit_bindgen::generate!({
    world: "wamn:receiving-component/receiving@0.1.0",
    inline: r#"
        package wamn:receiving-component@0.1.0;

        world receiving {
          import wamn:postgres/types@0.1.0;
          import wamn:postgres/statements@0.1.0;
          import wamn-receiving:receiving/record-receipt-pre-commit@1.0.0;
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
        "../generated/wit/deps/wamn-receiving-location",
        "../generated/wit/deps/wamn-receiving-purchase-order",
        "../generated/wit/deps/wamn-receiving-receipt",
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

receipt_codec::export_operation!(
    Component,
    exports::wamn_receiving::receiving::record_receipt,
    wamn::node::types,
    wamn_postgres_statements::Connection::new(),
    record_receipt_execute_with_context,
    receipt_codec
);

#[cfg(test)]
async fn record_receipt(
    input: Vec<contract::RecordReceiptItem>,
) -> Result<Vec<contract::RecordReceiptOutcome>, NodeError> {
    receipt_codec::validate(&input).map_err(|error| invalid_input(error.context()))?;
    Ok(receipt_codec::run(
        input,
        &mut wamn_postgres_statements::Connection::new(),
        record_receipt_execute,
    )
    .await)
}

#[cfg(test)]
async fn record_receipt_execute(
    connection: &mut wamn_postgres_statements::Connection,
    request: contract::RecordReceiptRequest,
) -> Result<contract::RecordReceiptResult, contract::RecordReceiptError> {
    let command = record_receipt_command(request);
    receipt::execute(connection, &command)
        .await
        .map(|value| contract::RecordReceiptResult {
            receipt_id: value.receipt_id.into(),
            purchase_order_id: value.purchase_order_id.into(),
            purchase_order_status: value.purchase_order_status.as_str().to_owned(),
            row_version: value.row_version,
        })
        .map_err(|error| map_record_receipt_error(&error))
}

fn record_receipt_command(request: contract::RecordReceiptRequest) -> receipt::RecordReceiptValue {
    receipt::RecordReceiptValue {
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
    }
}

fn map_record_receipt_error(error: &receipt::RecordReceiptError) -> contract::RecordReceiptError {
    receipt_codec::map_error(error.kind().literal(), |key| match key {
        "field" => error.field().map(str::to_owned),
        "id" => error.id().map(str::to_owned),
        "minimum" => error.minimum().map(|value| value.to_string()),
        "maximum" => error.maximum().map(|value| value.to_string()),
        "observed" => error.observed().map(|value| value.to_string()),
        "constraint" => error.constraint().map(str::to_owned),
        "operation" => Some("receiving.record_receipt".to_owned()),
        _ => None,
    })
}

async fn record_receipt_execute_with_context(
    context: wamn::node::types::NodeContext,
    connection: &mut wamn_postgres_statements::Connection,
    request: contract::RecordReceiptRequest,
) -> Result<contract::RecordReceiptResult, contract::RecordReceiptError> {
    let participation = wamn_postgres_statements::participation()
        .await
        .map_err(receipt::RecordReceiptError::participation_statement_failed)
        .map_err(|error| map_record_receipt_error(&error))?;
    let command = record_receipt_command(request);
    let result = match participation {
        Some(participation) => receipt::execute_with_pre_commit(
            connection,
            &command,
            &participation.operation,
            &participation.intent,
            |request| async move {
                let request = wamn_receiving::receiving::record_receipt_pre_commit::RecordReceiptPreCommitRequest {
                    receipt_id: request.receipt_id.into(),
                    purchase_order_id: request.purchase_order_id.into(),
                };
                wamn_receiving::receiving::record_receipt_pre_commit::run(context, request)
                    .await
                    .map(|_| ())
                    .map_err(map_participant_error)
            },
        )
        .await,
        None => receipt::execute(connection, &command).await,
    };
    result
        .map(|value| contract::RecordReceiptResult {
            receipt_id: value.receipt_id.into(),
            purchase_order_id: value.purchase_order_id.into(),
            purchase_order_status: value.purchase_order_status.as_str().to_owned(),
            row_version: value.row_version,
        })
        .map_err(|error| map_record_receipt_error(&error))
}

fn map_participant_error(error: wamn::node::types::NodeError) -> receipt::RecordReceiptError {
    use receipt::RecordReceiptErrorKind as Kind;

    match error {
        NodeError::InvalidInput(detail) => {
            receipt::RecordReceiptError::participation_refused(detail.message)
        }
        NodeError::Retryable(detail) => {
            receipt::RecordReceiptError::participation_failed(Kind::Retry, detail.message)
        }
        NodeError::RateLimited(detail) => {
            receipt::RecordReceiptError::participation_failed(Kind::Retry, detail.detail.message)
        }
        NodeError::Cancelled => receipt::RecordReceiptError::participation_failed(
            Kind::Timeout,
            "record_receipt participant was cancelled",
        ),
        NodeError::Terminal(detail) if detail.code.as_deref() == Some("permission_denied") => {
            receipt::RecordReceiptError::participation_failed(
                Kind::PermissionDenied,
                detail.message,
            )
        }
        NodeError::Terminal(detail) => {
            receipt::RecordReceiptError::participation_failed(Kind::InternalError, detail.message)
        }
    }
}

#[cfg(test)]
fn invalid_input(message: &str) -> NodeError {
    NodeError::InvalidInput(wamn::node::types::ErrorDetail {
        message: message.to_owned(),
        code: Some("invalid_input".to_owned()),
    })
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
            input: Err(contract::InvalidInputDetail {
                field: "input".to_owned(),
                minimum: None,
                maximum: None,
                observed: None,
            }),
        });
        let mut call = std::pin::pin!(super::record_receipt(input));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(matches!(
            std::future::Future::poll(call.as_mut(), &mut context),
            std::task::Poll::Ready(Err(super::NodeError::InvalidInput(_)))
        ));
    }

    #[test]
    fn arbitrary_request_id_is_preserved_in_each_outcome() {
        let request_id = "submit receipt / café 🧾";
        let input = receipt_codec::decode(&format!(
            r#"[{{"request_id":{id},"value":{VALID_VALUE}}},{{"request_id":{id},"value":null}}]"#,
            id = serde_json::to_string(request_id).unwrap(),
        ))
        .unwrap();
        let mut state = ();
        let mut call = std::pin::pin!(receipt_codec::run(
            input,
            &mut state,
            async |(), request| {
                Ok(contract::RecordReceiptResult {
                    receipt_id: "00000000-0000-0000-0000-000000000006".to_owned(),
                    purchase_order_id: request.purchase_order_id,
                    purchase_order_status: "open".to_owned(),
                    row_version: 1,
                })
            }
        ));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let std::task::Poll::Ready(output) = std::future::Future::poll(call.as_mut(), &mut context)
        else {
            panic!("the test handler performs no I/O");
        };
        assert!(output[0].outcome.is_ok());
        assert!(output[1].outcome.is_err());
        assert!(output.iter().all(|item| item.request_id == request_id));
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
        assert_eq!(decoded[1].input.as_ref().unwrap_err().field, "input");
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
        assert!(decoded.iter().all(|item| {
            item.input
                .as_ref()
                .is_err_and(|detail| detail.field == "input")
        }));
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
