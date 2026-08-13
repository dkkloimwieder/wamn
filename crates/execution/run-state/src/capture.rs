//! Node-level I/O capture for an admitted run.
//!
//! Capture mode is a run admission fact, not part of an authored flow. `full`
//! stores scrubbed input and output; `off` stores no node I/O facts. The output
//! ceiling is applied only while writing. Reads derive an oversized-output
//! projection from the durable `(output_json, output_size, payload_hash)` facts
//! and never consult the current ceiling.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The fixed maximum serialized output size retained in the node projection.
///
/// The size is a write-side storage bound. It must not be consulted when old
/// rows are projected because changing it must not reclassify history.
pub const OUTPUT_CAPTURE_CEILING_BYTES: usize = 64 * 1024;

/// The effective capture mode pinned on an admitted run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureMode {
    Full,
    Off,
}

impl CaptureMode {
    /// Parse the exact persisted capture-mode literal.
    pub fn from_sql(value: &str) -> Option<Self> {
        match value {
            "full" => Some(Self::Full),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    /// Return the persisted capture-mode literal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Off => "off",
        }
    }
}

impl fmt::Display for CaptureMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// JSON key-name fragments whose values are redacted wholesale.
const SECRET_KEY_FRAGMENTS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "authorization",
    "private_key",
    "credential",
];

/// String prefixes that identify secret-bearing values under otherwise safe keys.
const SECRET_VALUE_PREFIXES: &[&str] = &["Bearer ", "-----BEGIN", "AKIA"];

/// The placeholder replacing a redacted value.
pub const REDACTED: &str = "[redacted]";

/// FNV-1a 64 used for the optional persisted output content identity.
///
/// This hash is deterministic and dependency-free; it is not security-sensitive.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Recursively redact secret-bearing values in place.
pub fn scrub(value: &mut Value) -> bool {
    match value {
        Value::Object(map) => {
            let mut changed = false;
            for (key, value) in map.iter_mut() {
                if key_is_secret(key) {
                    *value = Value::String(REDACTED.to_string());
                    changed = true;
                } else {
                    changed |= scrub(value);
                }
            }
            changed
        }
        Value::Array(items) => {
            let mut changed = false;
            for item in items.iter_mut() {
                changed |= scrub(item);
            }
            changed
        }
        Value::String(string) if value_is_secret(string) => {
            *value = Value::String(REDACTED.to_string());
            true
        }
        _ => false,
    }
}

fn key_is_secret(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SECRET_KEY_FRAGMENTS
        .iter()
        .any(|fragment| lower.contains(fragment))
}

fn value_is_secret(value: &str) -> bool {
    SECRET_VALUE_PREFIXES
        .iter()
        .any(|prefix| value.starts_with(prefix))
}

/// The node projection facts written for one execution.
///
/// Under `full`, `output_size` is always present and `output_json` is absent
/// exactly when the serialized output exceeded [`OUTPUT_CAPTURE_CEILING_BYTES`].
/// Under `off`, every field is absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    pub output_json: Option<String>,
    pub input_json: Option<String>,
    pub output_size: Option<i64>,
    pub payload_hash: Option<String>,
}

impl Captured {
    fn off() -> Self {
        Self {
            output_json: None,
            input_json: None,
            output_size: None,
            payload_hash: None,
        }
    }
}

/// Apply the admitted capture mode to one node execution.
pub fn derive(mode: CaptureMode, output: &Value, input: &Value) -> Captured {
    if mode == CaptureMode::Off {
        return Captured::off();
    }

    let raw_output = output.to_string();
    let output_size = i64::try_from(raw_output.len()).unwrap_or(i64::MAX);
    let payload_hash = format!("{:016x}", fnv1a64(raw_output.as_bytes()));

    let mut scrubbed_output = output.clone();
    scrub(&mut scrubbed_output);
    let scrubbed_output = scrubbed_output.to_string();

    let mut scrubbed_input = input.clone();
    scrub(&mut scrubbed_input);

    Captured {
        output_json: (raw_output.len() <= OUTPUT_CAPTURE_CEILING_BYTES).then_some(scrubbed_output),
        input_json: Some(scrubbed_input.to_string()),
        output_size: Some(output_size),
        payload_hash: Some(payload_hash),
    }
}

/// Public node-output view derived from persisted projection facts.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum NodeOutputProjection {
    Output(Value),
    OutputTooLarge(OutputTooLarge),
}

/// Typed metadata exposed when the captured output exceeded the write ceiling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutputTooLarge {
    kind: OutputTooLargeKind,
    pub size: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
enum OutputTooLargeKind {
    #[serde(rename = "output-too-large")]
    OutputTooLarge,
}

/// Project a node's output using only its durable facts.
///
/// `output = NULL` plus a recorded size is the complete oversized-output
/// discriminator. `output = NULL` plus no size is capture-off and projects no
/// output. The current write ceiling is deliberately not an argument.
pub fn project_output(
    output: Option<Value>,
    output_size: Option<i64>,
    payload_hash: Option<String>,
) -> Option<NodeOutputProjection> {
    match (output, output_size) {
        (Some(output), _) => Some(NodeOutputProjection::Output(output)),
        (None, Some(size)) => Some(NodeOutputProjection::OutputTooLarge(OutputTooLarge {
            kind: OutputTooLargeKind::OutputTooLarge,
            size,
            hash: payload_hash,
        })),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn scrub_redacts_secret_keys_and_value_shapes() {
        let mut value = json!({
            "PassWord": "hunter2",
            "header": "Bearer eyJabc.def.ghi",
            "nested": [{"api_key": "key"}],
            "plain": "visible",
        });
        assert!(scrub(&mut value));
        assert_eq!(value["PassWord"], json!(REDACTED));
        assert_eq!(value["header"], json!(REDACTED));
        assert_eq!(value["nested"][0]["api_key"], json!(REDACTED));
        assert_eq!(value["plain"], json!("visible"));
    }

    #[test]
    fn full_scrubs_stored_input_and_output() {
        let captured = derive(
            CaptureMode::Full,
            &json!({"token": "output-secret", "value": "ok"}),
            &json!({"password": "input-secret"}),
        );
        let stored = format!(
            "{}{}",
            captured.output_json.as_deref().expect("stored output"),
            captured.input_json.as_deref().expect("stored input")
        );
        assert!(!stored.contains("output-secret"));
        assert!(!stored.contains("input-secret"));
        assert!(stored.contains(REDACTED));
        assert!(captured.output_size.is_some());
    }

    #[test]
    fn off_records_no_node_io_facts() {
        assert_eq!(
            derive(CaptureMode::Off, &json!({"a": 1}), &json!({"b": 2})),
            Captured::off()
        );
    }

    #[test]
    fn over_ceiling_output_records_size_and_projects_typed_metadata() {
        let output = json!({"blob": "x".repeat(OUTPUT_CAPTURE_CEILING_BYTES)});
        let raw = output.to_string();
        assert!(raw.len() > OUTPUT_CAPTURE_CEILING_BYTES);

        let captured = derive(CaptureMode::Full, &output, &json!({"token": "secret"}));
        assert_eq!(captured.output_json, None);
        assert_eq!(captured.output_size, Some(raw.len() as i64));
        assert!(captured.input_json.is_some());

        let projected = project_output(None, captured.output_size, captured.payload_hash.clone())
            .expect("oversized output projects metadata");
        assert_eq!(
            serde_json::to_value(projected).expect("projection serializes"),
            json!({
                "kind": "output-too-large",
                "size": raw.len() as i64,
                "hash": captured.payload_hash.expect("captured hash"),
            })
        );
    }

    #[test]
    fn projection_uses_persisted_facts_without_a_ceiling() {
        assert_eq!(
            project_output(Some(json!({"ok": true})), Some(1), None),
            Some(NodeOutputProjection::Output(json!({"ok": true})))
        );
        assert_eq!(project_output(None, None, None), None);
        assert_eq!(
            serde_json::to_value(project_output(None, Some(7), None).unwrap()).unwrap(),
            json!({"kind": "output-too-large", "size": 7})
        );
    }

    #[test]
    fn capture_mode_parsing_is_exact() {
        assert_eq!(CaptureMode::from_sql("full"), Some(CaptureMode::Full));
        assert_eq!(CaptureMode::from_sql("off"), Some(CaptureMode::Off));
        assert_eq!(CaptureMode::from_sql("preview"), None);
        assert_eq!(CaptureMode::from_sql("FULL"), None);
    }
}
