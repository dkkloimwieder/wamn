//! Store fixture items under their stable upstream keys.
#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits equal length and capacity"
)]

wit_bindgen::generate!({
    world: "rest-consumer-fixture:component/rest-consumer-fixture@0.1.0",
    path: [
        "../../../crates/execution/workflow/router/wit",
        "../../../crates/platform/runtime/wit/deps/wamn-postgres-0.4",
        "../../target/wamn/rest_consumer_fixture/wit",
        "wit",
    ],
    generate_all,
    async: true,
});

struct Component;
export!(Component);

use exports::rest_consumer_fixture::item::record as contract;
mod codec {
    use super::contract;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/rest_consumer_fixture/wit/item_record_codec.rs"
    ));
}
mod statements {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/rest_consumer_fixture/wamn/item_record.rs"
    ));
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    key: String,
    name: String,
}

fn items(value: &str) -> Result<Vec<Item>, contract::RecordError> {
    let items: Vec<Item> = serde_json::from_str(value)
        .map_err(|_| codec::map_error("invalid_input", |_| Some("value.items".into())))?;
    if items
        .iter()
        .any(|item| item.key.is_empty() || item.name.is_empty())
    {
        return Err(codec::map_error("invalid_input", |_| {
            Some("value.items".into())
        }));
    }
    Ok(items)
}

async fn handle(
    transaction: &mut wamn_postgres_statements::Transaction,
    request: contract::RecordRequest,
) -> Result<contract::RecordResult, contract::RecordError> {
    use wamn_postgres_statements::StatementErrorType;
    let mut keys = Vec::new();
    for item in items(&request.items)? {
        let row = statements::record_item(transaction, item.key.clone(), item.name)
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
        keys.push(row.map_or(item.key, |row| row.key));
    }
    Ok(contract::RecordResult {
        keys: serde_json::json!(keys).to_string(),
    })
}
codec::export_operation!(Component, contract, wamn::node::types, handle, codec);

#[cfg(test)]
mod tests {
    #[test]
    fn an_empty_batch_succeeds_and_malformed_items_refuse() {
        assert!(super::items("[]").unwrap().is_empty());
        assert!(super::items(r#"[{"key":"1","name":"one"}]"#).is_ok());
        assert!(super::items(r#"[{"key":"1"}]"#).is_err());
        assert!(super::items(r#"[{"key":"","name":"one"}]"#).is_err());
    }
}
