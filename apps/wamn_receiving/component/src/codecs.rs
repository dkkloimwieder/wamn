//! Typed purchase-order update boundary over the existing application operation.

use super::exports::wamn_receiving::purchase_order::update as contract;
#[cfg(test)]
use super::{NodeError, invalid_input};
use wamn_receiving_data_access::purchase_order;

pub(super) mod codec {
    use super::contract;
    include!("../../generated/wit/purchase_order_update_codec.rs");
}

#[cfg(test)]
pub(super) async fn run(
    input: Vec<contract::UpdateItem>,
) -> Result<Vec<contract::UpdateOutcome>, NodeError> {
    codec::validate(&input).map_err(|error| invalid_input(error.context()))?;
    Ok(codec::run(
        input,
        &mut wamn_postgres_statements::Connection::new(),
        execute,
    )
    .await)
}

pub(super) async fn execute(
    connection: &mut wamn_postgres_statements::Connection,
    request: contract::UpdateRequest,
) -> Result<contract::UpdateResult, contract::UpdateError> {
    let supplier_id = match request.change.supplier_id {
        None => purchase_order::SupplierIdUpdate::Omitted,
        Some(None) => purchase_order::SupplierIdUpdate::Null,
        Some(Some(value)) => purchase_order::SupplierIdUpdate::Value(value.into()),
    };
    purchase_order::update(
        connection,
        &request.id,
        request.expected_row_version,
        supplier_id,
    )
    .await
    .map(|row| codec::row!(row, contract::UpdateResult))
    .map_err(|error| {
        codec::map_error(error.kind().literal(), |key| {
            super::reads::error_detail(
                &error,
                key,
                "purchase_order.update",
                Some(("id", &request.id)),
                Some(request.expected_row_version),
            )
        })
    })
}

codec::export_operation!(
    super::Component,
    super::exports::wamn_receiving::purchase_order::update,
    super::wamn::node::types,
    wamn_postgres_statements::Connection::new(),
    execute,
    codec
);

#[cfg(test)]
mod tests {
    use super::{codec, contract};

    fn decode(change: &str, revision: &str) -> contract::UpdateRequest {
        let input = format!(
            r#"[{{"request_id":"update-1","id":"00000000-0000-0000-0000-000000000001","expected_row_version":{revision},"change":{change}}}]"#
        );
        codec::decode(&input).unwrap().pop().unwrap().input.unwrap()
    }

    fn refusal(change: &str, revision: &str) -> serde_json::Value {
        let input = format!(
            r#"[{{"request_id":"update-1","id":"00000000-0000-0000-0000-000000000001","expected_row_version":{revision},"change":{change}}}]"#
        );
        let item = codec::decode(&input).unwrap().pop().unwrap();
        serde_json::to_value(item.input.expect_err("the request refuses").field).unwrap()
    }

    /// A revision is an int32, so it is one JSON number with one spelling, and
    /// a value outside that width refuses instead of wrapping.
    #[test]
    fn a_revision_is_one_number_inside_its_width() {
        assert_eq!(decode("{}", "1").expected_row_version, 1);
        assert_eq!(decode("{}", "2147483647").expected_row_version, i32::MAX);
        assert_eq!(refusal("{}", "2147483648"), "expected_row_version");
        assert_eq!(refusal("{}", r#""1""#), "input");

        let output = [contract::UpdateOutcome {
            request_id: "conflict".to_owned(),
            outcome: Err(contract::UpdateError::ConcurrencyConflict(
                contract::ConcurrencyConflictDetail {
                    expected_row_version: i32::MAX,
                    observed_row_version: 7,
                },
            )),
        }];
        let encoded: serde_json::Value = serde_json::from_str(&codec::encode(&output)).unwrap();
        assert_eq!(encoded[0]["request_id"], "conflict");
        assert_eq!(
            encoded[0]["error"]["detail"]["expected_row_version"],
            2_147_483_647
        );
        assert_eq!(encoded[0]["error"]["detail"]["observed_row_version"], 7);
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
            {"request_id":"null","id":"00000000-0000-0000-0000-000000000001","expected_row_version":1,"change":{"supplier_id":null}},
            {"request_id":"unknown","id":"00000000-0000-0000-0000-000000000001","expected_row_version":1,"change":{"status":"complete"}},
            {"request_id":"wrong-type","id":"00000000-0000-0000-0000-000000000001","expected_row_version":"1","change":{}}
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
