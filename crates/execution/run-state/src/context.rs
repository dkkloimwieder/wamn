//! Durable run-context checkpoint encoding.

use serde_json::{Map, Value};

/// A malformed durable context checkpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextStateError {
    value: Value,
}

impl std::fmt::Display for ContextStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "run checkpoint context must be an object, got {}",
            self.value
        )
    }
}

impl std::error::Error for ContextStateError {}

/// Read the durable context document from `state_json`.
///
/// A missing checkpoint is a fresh empty context. A present non-object value is
/// rejected so corruption cannot silently change replacement into merge
/// semantics.
pub fn read(state: Option<&Value>) -> Result<Value, ContextStateError> {
    let context = state
        .and_then(|value| value.get("context"))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    if context.is_object() {
        Ok(context)
    } else {
        Err(ContextStateError { value: context })
    }
}

/// Return `state_json` with its whole context document replaced.
///
/// Retry and wake cursors sharing the checkpoint remain untouched.
pub fn replace(state: Option<Value>, context: Value) -> Result<Value, ContextStateError> {
    if !context.is_object() {
        return Err(ContextStateError { value: context });
    }
    let mut state = match state {
        Some(Value::Object(state)) => state,
        _ => Map::new(),
    };
    state.insert("context".to_string(), context);
    Ok(Value::Object(state))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn replacement_preserves_other_checkpoint_cursors_without_merging_context() {
        let state = json!({
            "context": {"dropped": true},
            "retry": {"node": "effect", "attempt": 2}
        });
        let replaced = replace(Some(state), json!({"kept": true})).unwrap();
        assert_eq!(replaced["context"], json!({"kept": true}));
        assert!(replaced["context"].get("dropped").is_none());
        assert_eq!(replaced["retry"]["attempt"], 2);
    }

    #[test]
    fn malformed_context_is_detected() {
        assert!(read(Some(&json!({"context": ["not", "a", "document"]}))).is_err());
        assert!(replace(None, json!(null)).is_err());
    }
}
