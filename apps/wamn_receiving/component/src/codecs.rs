//! The generated codecs, tested at the component.

use crate::exports::wamn_receiving::purchase_order::update as contract;
use crate::generated::purchase_order::update::codec;
use crate::{NodeError, invalid_input};

async fn run(input: Vec<contract::UpdateItem>) -> Result<Vec<contract::UpdateOutcome>, NodeError> {
    codec::validate(&input).map_err(|error| invalid_input(error.context()))?;
    Ok(codec::run(input, &mut (), async |(), _| {
        unreachable!("a refused item reaches no handler")
    })
    .await)
}

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
    let mut call = std::pin::pin!(run(input));
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    let std::task::Poll::Ready(Ok(output)) = std::future::Future::poll(call.as_mut(), &mut context)
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

/// A supplier name that is blank after a trim refuses in the codec, on its
/// field, before the CHECK that guards the same rule could answer.
#[test]
fn a_blank_supplier_name_refuses_on_its_field() {
    use crate::exports::wamn_receiving::supplier::create as contract;
    use crate::generated::supplier::create::codec as create;
    let input = create::decode(
        r#"[
            {"request_id":"empty","idempotency_key":"k1","name":""},
            {"request_id":"spaces","idempotency_key":"k2","name":"  "},
            {"request_id":"tab","idempotency_key":"k3","name":"\t"}
        ]"#,
    )
    .unwrap();
    // Each item refuses before the handler runs, so the test needs no
    // database.
    let mut state = ();
    let mut call = std::pin::pin!(create::run(input, &mut state, async |(), _| unreachable!(
        "a refused item reaches no handler"
    )));
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    let std::task::Poll::Ready(output) = std::future::Future::poll(call.as_mut(), &mut context)
    else {
        panic!("a refused item performs no I/O");
    };
    assert_eq!(output.len(), 3);
    for outcome in output {
        let Err(contract::CreateError::InvalidInput(detail)) = outcome.outcome else {
            panic!("{} refuses as invalid input", outcome.request_id);
        };
        assert_eq!(detail.field, "name", "{}", outcome.request_id);
    }
}
