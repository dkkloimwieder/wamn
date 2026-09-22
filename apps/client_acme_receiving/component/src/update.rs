//! Typed Acme purchase-order update boundary over its package-owned SQL.

use super::exports::client_acme_receiving::purchase_order::update as contract;
use super::{AccessError, access_detail};
use wamn_client_acme_receiving_data_access::operation;

pub(super) mod codec {
    use super::contract;

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wit/purchase_order_update_codec.rs"
    ));
}

pub(super) async fn handle(
    connection: &mut wamn_postgres_statements::Connection,
    request: contract::UpdateRequest,
) -> Result<contract::UpdateResult, contract::UpdateError> {
    operation::purchase_order_update(
        connection,
        &request.id,
        request.expected_row_version,
        request.change.acme_inspection_required,
        request.change.acme_quality_status,
    )
    .await
    .map(|row| codec::row!(row, contract::UpdateResult))
    .map_err(|error| map_error(&error, &request.id, i64::from(request.expected_row_version)))
}

fn map_error(error: &AccessError, id: &str, expected: i64) -> contract::UpdateError {
    codec::map_error(error.kind().literal(), |key| {
        access_detail(
            error,
            key,
            "purchase_order.update",
            Some(("id", id)),
            Some(expected),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{codec, contract};

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
}
