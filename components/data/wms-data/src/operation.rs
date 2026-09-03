//! The array envelope, and the one operation this crate exports.
//!
//! Every registered operation takes an array and returns a correlated array,
//! and each outer item executes INDEPENDENTLY under `per_input` semantics: one
//! item's refusal does not roll back its neighbours, because each holds its
//! own transaction. Cross-input atomicity is deferred platform-wide.

use serde::{Deserialize, Serialize};

use crate::error::AccessError;
use crate::inventory_move::{self, MoveCommand};

/// Hard bound from the operation contract's envelope declaration.
const MAX_ITEMS: usize = 100;

/// A whole-invocation failure — the envelope itself was not admissible, so no
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
    value: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ItemResult {
    Succeeded {
        request_id: String,
        value: serde_json::Value,
    },
    Refused {
        request_id: String,
        error: serde_json::Value,
    },
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

fn refused(request_id: String, error: &AccessError) -> ItemResult {
    ItemResult::Refused {
        request_id,
        error: serde_json::json!({
            "code": error.kind().literal(),
            "detail": error.detail(),
        }),
    }
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
    let mut output: Vec<ItemResult> = Vec::with_capacity(items.len());
    for item in items {
        let command: MoveCommand = match serde_json::from_value(item.value) {
            Ok(command) => command,
            Err(error) => {
                output.push(ItemResult::Refused {
                    request_id: item.request_id,
                    error: serde_json::json!({
                        "code": "invalid_input",
                        "detail": { "field": error.to_string() },
                    }),
                });
                continue;
            }
        };
        match inventory_move::execute(&command).await {
            Ok(value) => output.push(ItemResult::Succeeded {
                request_id: item.request_id,
                value: value.to_json(),
            }),
            Err(error) => output.push(refused(item.request_id, &error)),
        }
    }
    Ok(serde_json::to_string(&output).expect("closed operation results always serialize"))
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
    /// runs — a caller learns the envelope was too large rather than watching
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
}
