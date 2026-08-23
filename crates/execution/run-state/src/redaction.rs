//! The JSON payload redaction policy: the secret vocabularies and the tree
//! walk that applies them.
//!
//! Extracted by wamn-0h0g.26.2 from the retired node I/O capture module, whose
//! durable grain died with the `node_runs` projection. The policy is the half
//! that survives, and its one intended consumer is wamn-0h0g.24.5 — the
//! router-edge live view, which publishes bounded, redacted payload previews on
//! its own subject. That bead's input contract is this module exactly as
//! extracted: [`scrub`]'s recursion, the nine secret key fragments, the three
//! secret value prefixes, and [`OUTPUT_CAPTURE_CEILING_BYTES`]. A live view
//! that needs more renegotiates it there rather than widening the policy here.
//!
//! This is a known-pattern redaction floor, not a secret-classification
//! guarantee. The retired capture module derived a content hash over the
//! PRE-scrub bytes; that hash did not survive, and it should not be revived as
//! written: an identity published beside a redacted payload has to be computed
//! over the scrubbed value or it re-exposes what the scrub removed.

use serde_json::Value;

/// The maximum serialized payload size a redacted preview retains.
pub const OUTPUT_CAPTURE_CEILING_BYTES: usize = 64 * 1024;

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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn scrub_redacts_secret_keys_and_value_shapes() {
        let mut value = json!({
            "PassWord": "hunter2",
            "passwd": "hunter2",
            "secret": "hunter2",
            "token": "hunter2",
            "api_key": "hunter2",
            "apikey": "hunter2",
            "authorization": "hunter2",
            "private_key": "hunter2",
            "credential": "hunter2",
            "header": "Bearer eyJabc.def.ghi",
            "pem": "-----BEGIN RSA PRIVATE KEY-----",
            "access": "AKIAIOSFODNN7EXAMPLE",
            "nested": [{"api_key": "key"}],
            "plain": "visible",
        });
        assert!(scrub(&mut value));
        for key in [
            "PassWord",
            "passwd",
            "secret",
            "token",
            "api_key",
            "apikey",
            "authorization",
            "private_key",
            "credential",
            "header",
            "pem",
            "access",
        ] {
            assert_eq!(value[key], json!(REDACTED), "{key} survived the scrub");
        }
        assert_eq!(value["nested"][0]["api_key"], json!(REDACTED));
        assert_eq!(value["plain"], json!("visible"));
    }
}
