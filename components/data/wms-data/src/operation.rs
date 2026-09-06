//! The array envelope, and the seven operations this crate exports.
//!
//! Every registered operation takes an array and returns a correlated array,
//! and each outer item executes INDEPENDENTLY under `per_input` semantics: one
//! item's refusal does not roll back its neighbours, because each holds its
//! own transaction. Cross-input atomicity is deferred platform-wide.
//!
//! Two item shapes share the envelope. A command item carries its body under
//! `value`, the member its canonicalization contract keys; a read item IS its
//! body, beside the `request_id` that correlates the answer.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use wamn_postgres_statements::Connection;

use crate::error::{AccessError, AccessErrorKind};
use crate::{
    inventory_adjust, inventory_aggregate, inventory_merge, inventory_move, inventory_split, pallet,
};

/// Hard bound from the operation contracts' envelope declaration.
const MAX_ITEMS: usize = 100;

/// A whole-invocation failure -- the envelope itself was not admissible, so no
/// item ran and there is nothing to correlate.
#[derive(Debug)]
pub struct InvocationError {
    code: &'static str,
    context: String,
}

impl InvocationError {
    fn new(code: &'static str, context: impl Into<String>) -> Self {
        Self {
            code,
            context: context.into(),
        }
    }

    /// Stable refusal literal.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// What was wrong with the envelope.
    #[must_use]
    pub fn context(&self) -> &str {
        &self.context
    }
}

#[derive(Debug, Deserialize)]
struct EnvelopeItem {
    request_id: String,
    #[serde(flatten)]
    body: Map<String, Value>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ItemResult {
    Succeeded { request_id: String, value: Value },
    Refused { request_id: String, error: Value },
}

fn prepare_envelope(input: &str) -> Result<Vec<EnvelopeItem>, InvocationError> {
    let items: Vec<EnvelopeItem> = serde_json::from_str(input)
        .map_err(|error| InvocationError::new("invalid_input", error.to_string()))?;
    if items.is_empty() {
        return Err(InvocationError::new(
            "invalid_input",
            "an envelope carries at least one item",
        ));
    }
    if items.len() > MAX_ITEMS {
        return Err(InvocationError::new(
            "invalid_input",
            format!("an envelope carries at most {MAX_ITEMS} items"),
        ));
    }
    Ok(items)
}

/// A body that is not what the contract admits, naming what the decoder
/// stumbled on.
fn malformed(error: &serde_json::Error) -> AccessError {
    AccessError::new(
        AccessErrorKind::InvalidInput,
        serde_json::json!({ "field": error.to_string() }),
    )
}

/// The command body: the item's `value` member.
fn command<T: DeserializeOwned>(mut body: Map<String, Value>) -> Result<T, AccessError> {
    let value = body.remove("value").unwrap_or(Value::Null);
    serde_json::from_value(value).map_err(|error| malformed(&error))
}

/// The read body: the item itself, less its correlation id.
fn read<T: DeserializeOwned>(body: Map<String, Value>) -> Result<T, AccessError> {
    serde_json::from_value(Value::Object(body)).map_err(|error| malformed(&error))
}

fn outcome(request_id: String, result: Result<Value, AccessError>) -> ItemResult {
    match result {
        Ok(value) => ItemResult::Succeeded { request_id, value },
        Err(error) => ItemResult::Refused {
            request_id,
            error: serde_json::json!({
                "code": error.kind().literal(),
                "detail": error.detail(),
            }),
        },
    }
}

fn serialized(output: &[ItemResult]) -> String {
    serde_json::to_string(output).expect("closed operation results always serialize")
}

/// Execute only `inventory.move`.
///
/// # Errors
///
/// [`InvocationError`] when the ENVELOPE is inadmissible. A single item's
/// refusal is carried in that item's result instead, so its neighbours still
/// report what they did.
pub async fn inventory_move_operation(input: &str) -> Result<String, InvocationError> {
    let items = prepare_envelope(input)?;
    let mut output = Vec::with_capacity(items.len());
    for item in items {
        let result = match command::<inventory_move::MoveCommand>(item.body) {
            Ok(command) => inventory_move::execute(&command)
                .await
                .map(|value| value.to_json()),
            Err(error) => Err(error),
        };
        output.push(outcome(item.request_id, result));
    }
    Ok(serialized(&output))
}

/// Execute only `inventory.adjust`.
///
/// # Errors
///
/// [`InvocationError`] when the ENVELOPE is inadmissible.
pub async fn inventory_adjust_operation(input: &str) -> Result<String, InvocationError> {
    let items = prepare_envelope(input)?;
    let mut output = Vec::with_capacity(items.len());
    for item in items {
        let result = match command::<inventory_adjust::AdjustCommand>(item.body) {
            Ok(command) => inventory_adjust::execute(&command)
                .await
                .map(|value| value.to_json()),
            Err(error) => Err(error),
        };
        output.push(outcome(item.request_id, result));
    }
    Ok(serialized(&output))
}

/// Execute only `inventory.merge`.
///
/// # Errors
///
/// [`InvocationError`] when the ENVELOPE is inadmissible.
pub async fn inventory_merge_operation(input: &str) -> Result<String, InvocationError> {
    let items = prepare_envelope(input)?;
    let mut output = Vec::with_capacity(items.len());
    for item in items {
        let result = match command::<inventory_merge::MergeCommand>(item.body) {
            Ok(command) => inventory_merge::execute(&command)
                .await
                .map(|value| value.to_json()),
            Err(error) => Err(error),
        };
        output.push(outcome(item.request_id, result));
    }
    Ok(serialized(&output))
}

/// Execute only `inventory.split`.
///
/// # Errors
///
/// [`InvocationError`] when the ENVELOPE is inadmissible.
pub async fn inventory_split_operation(input: &str) -> Result<String, InvocationError> {
    let items = prepare_envelope(input)?;
    let mut output = Vec::with_capacity(items.len());
    for item in items {
        let result = match command::<inventory_split::SplitCommand>(item.body) {
            Ok(command) => inventory_split::execute(&command)
                .await
                .map(|value| value.to_json()),
            Err(error) => Err(error),
        };
        output.push(outcome(item.request_id, result));
    }
    Ok(serialized(&output))
}

/// Execute only `inventory.aggregate`.
///
/// # Errors
///
/// [`InvocationError`] when the ENVELOPE is inadmissible.
pub async fn inventory_aggregate_operation(input: &str) -> Result<String, InvocationError> {
    let items = prepare_envelope(input)?;
    let mut connection = Connection::new();
    let mut output = Vec::with_capacity(items.len());
    for item in items {
        let result = match read::<inventory_aggregate::AggregateInput>(item.body) {
            Ok(_) => inventory_aggregate::execute(&mut connection)
                .await
                .map(|rows| inventory_aggregate::rows_to_json(&rows)),
            Err(error) => Err(error),
        };
        output.push(outcome(item.request_id, result));
    }
    Ok(serialized(&output))
}

/// Execute only `pallet.get`.
///
/// # Errors
///
/// [`InvocationError`] when the ENVELOPE is inadmissible.
pub async fn pallet_get_operation(input: &str) -> Result<String, InvocationError> {
    let items = prepare_envelope(input)?;
    let mut connection = Connection::new();
    let mut output = Vec::with_capacity(items.len());
    for item in items {
        let result = match read::<pallet::GetInput>(item.body) {
            Ok(input) => pallet::get(&mut connection, &input.id)
                .await
                .map(|row| pallet::row_to_json(&row)),
            Err(error) => Err(error),
        };
        output.push(outcome(item.request_id, result));
    }
    Ok(serialized(&output))
}

/// Execute only `pallet.query`.
///
/// # Errors
///
/// [`InvocationError`] when the ENVELOPE is inadmissible.
pub async fn pallet_query_operation(input: &str) -> Result<String, InvocationError> {
    let items = prepare_envelope(input)?;
    let mut connection = Connection::new();
    let mut output = Vec::with_capacity(items.len());
    for item in items {
        let result = match read::<pallet::QueryInput>(item.body) {
            Ok(input) => pallet::query(&mut connection, &input)
                .await
                .map(|page| pallet::page_to_json(&page)),
            Err(error) => Err(error),
        };
        output.push(outcome(item.request_id, result));
    }
    Ok(serialized(&output))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An envelope that is not an array is a WHOLE-INVOCATION refusal: there
    /// are no items to correlate a per-item error with.
    #[test]
    fn a_non_array_envelope_refuses_the_invocation() {
        let error = prepare_envelope("{}").expect_err("an object is not an envelope");
        assert_eq!(error.code(), "invalid_input");
    }

    #[test]
    fn an_empty_envelope_refuses() {
        assert!(prepare_envelope("[]").is_err());
    }

    /// The bound is the contract's, and exceeding it refuses before any item
    /// runs -- a caller learns the envelope was too large rather than watching
    /// the first hundred succeed.
    #[test]
    fn an_oversized_envelope_refuses_before_any_item_runs() {
        let item = r#"{"request_id":"r","value":{}}"#;
        let oversized = format!("[{}]", vec![item; MAX_ITEMS + 1].join(","));
        let error = prepare_envelope(&oversized).expect_err("101 items refuse");
        assert!(error.context().contains("100"), "{}", error.context());

        let exact = format!("[{}]", vec![item; MAX_ITEMS].join(","));
        assert!(prepare_envelope(&exact).is_ok(), "100 items are admissible");
    }

    /// A missing correlation id refuses the whole envelope; a malformed body
    /// refuses only its item, naming what was wrong.
    #[test]
    fn the_correlation_id_is_the_envelopes_and_the_body_is_the_items() {
        assert!(prepare_envelope(r#"[{"id":"x"}]"#).is_err());

        let items = prepare_envelope(
            r#"[{"request_id":"a","id":"x","extra":true},{"request_id":"b","value":{}}]"#,
        )
        .unwrap();
        assert_eq!(items[0].request_id, "a");
        let error = read::<pallet::GetInput>(items[0].body.clone()).unwrap_err();
        assert_eq!(error.kind(), AccessErrorKind::InvalidInput);
        let error = command::<inventory_move::MoveCommand>(items[1].body.clone()).unwrap_err();
        assert_eq!(error.kind(), AccessErrorKind::InvalidInput);
        assert!(
            error.detail()["field"]
                .as_str()
                .unwrap()
                .contains("missing field")
        );
        assert!(command::<inventory_move::MoveCommand>(items[0].body.clone()).is_err());
    }

    #[test]
    fn a_refusal_serializes_as_code_and_detail_beside_its_request_id() {
        let output = [outcome(
            "r".to_owned(),
            Err(AccessError::missing(AccessErrorKind::NotFound, "id", "x")),
        )];
        assert_eq!(
            serialized(&output),
            r#"[{"request_id":"r","error":{"code":"not_found","detail":{"field":"id","id":"x"}}}]"#
        );
    }
}
