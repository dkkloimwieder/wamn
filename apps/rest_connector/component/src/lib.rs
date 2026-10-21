//! Store fixture items under their stable upstream keys.
#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits equal length and capacity"
)]

wit_bindgen::generate!({
    world: "rest-connector:component/rest-connector@0.1.0",
    path: [
        "../../../crates/execution/workflow/router/wit",
        "../../../crates/platform/runtime/wit/deps/wamn-flow-http-routing",
        "../../../crates/platform/runtime/wit/deps/wamn-postgres-0.4",
        "../../target/wamn/rest_connector/wit",
        "wit",
    ],
    generate_all,
    async: true,
});

struct Component;
export!(Component);

use exports::rest_connector::item::record as contract;
mod codec {
    use super::contract;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/rest_connector/wit/item_record_codec.rs"
    ));
}
mod statements {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/rest_connector/wamn/item_record.rs"
    ));
}

async fn handle(
    transaction: &mut wamn_postgres_statements::Transaction,
    request: contract::RecordRequest,
) -> Result<contract::RecordResult, contract::RecordError> {
    use wamn_postgres_statements::StatementErrorType;
    let row = statements::record_item(transaction, request.key.clone(), request.name)
        .await
        .map_err(|error| {
            let code = match error.type_() {
                StatementErrorType::SerializationFailure
                | StatementErrorType::ConnectionUnavailable => "retry",
                StatementErrorType::StatementTimeout => "timeout",
                _ => "internal_error",
            };
            codec::map_error(code, |_| None)
        })?;
    Ok(contract::RecordResult {
        key: row.map_or(request.key, |row| row.key),
    })
}
codec::export_operation!(Component, contract, wamn::node::types, handle, codec);
