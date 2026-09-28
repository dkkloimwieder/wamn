//! The generated codecs, tested at the component.

use crate::exports::client_acme_receiving::purchase_order::update as contract;
use crate::generated::purchase_order::update::codec;

fn decode(change: &str, revision: &str) -> contract::UpdateRequest {
    let input = format!(
        r#"[{{"request_id":"update-1","id":"00000000-0000-0000-0000-000000000001","expected_row_version":{revision},"change":{change}}}]"#
    );
    codec::decode(&input).unwrap().pop().unwrap().input.unwrap()
}

/// The purchase order revision follows its base column, which is int32.
#[test]
fn update_codec_preserves_the_revision_and_change_states() {
    assert_eq!(decode("{}", "1").expected_row_version, 1);
    assert_eq!(decode("{}", "2147483647").expected_row_version, i32::MAX);
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
        r#"[{"request_id":"wrong-owner","id":"00000000-0000-0000-0000-000000000001","expected_row_version":1,"change":{"supplier_id":"00000000-0000-0000-0000-000000000002"}}]"#,
    )
    .unwrap();
    let error = rejected[0].input.as_ref().unwrap_err();
    assert_eq!(error.field, "input");
}
