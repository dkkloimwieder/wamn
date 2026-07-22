//! Node-level I/O capture policy application (9.6): the pure logic that fills a
//! `node_runs` row's capture columns from a flow's [`Capture`] policy before the
//! `wamn:postgres` write. No DB, no wasm, no clock — the flowrunner guest links
//! it so the decision is unit-tested off-cluster, exactly as the SQL builders are.
//!
//! For each node execution the driver hands us the node's OUTPUT emission (the
//! reconstruction-relevant payload) and its INPUT (the partial-re-run seed).
//! [`derive`] folds the policy into the exact column values:
//!
//! - `output_json` / `input_json` — the STORED payloads: faithful ([`CaptureMode::Full`]),
//!   secret-scrubbed ([`CaptureMode::Scrubbed`]), or NULL ([`CaptureMode::Preview`] /
//!   [`CaptureMode::Off`], and any payload over the size threshold).
//! - `preview_head` — the first [`PREVIEW_CHARS`] chars of the SCRUBBED output
//!   serialization (always scrubbed, so the inspector never surfaces a raw secret,
//!   even in `full` mode).
//! - `payload_size` — the full serialized byte length of the output.
//! - `payload_hash` — [`fnv1a64`] over the output serialization (a content id).
//! - `capture_mode` — the EFFECTIVE mode literal ([`CaptureMode::as_str`]), which
//!   is `preview` whenever the size threshold forced preview-only storage.
//! - `redacted` — true iff the STORED payloads were scrubbed (`scrubbed` mode).
//!
//! v0 does not offload the payload BYTES to the object store (5.10 owns that seam,
//! `input_ref`/`output_ref`); an oversized payload is truncated to preview-only
//! in Postgres, and the reserved `*_ref` columns stay null.

use serde_json::Value;
use wamn_flow::{Capture, CaptureMode};

/// The leading chars of the scrubbed output serialization kept in `preview_head`
/// (the editor inspection snippet, and all a `preview`-mode row retains of its
/// payload). Chars, not bytes — the cut is on a UTF-8 boundary.
pub const PREVIEW_CHARS: usize = 256;

/// JSON key-name fragments (case-insensitive substring) whose value is redacted
/// wholesale — v0's fixed vocabulary of common secret-bearing key names. No regex,
/// no heavy deps: guest-compilable.
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

/// Value-shape prefixes that mark a STRING leaf as secret-bearing regardless of
/// its key: an HTTP `Bearer ` token, a PEM block, an AWS access-key id (`AKIA…`).
const SECRET_VALUE_PREFIXES: &[&str] = &["Bearer ", "-----BEGIN", "AKIA"];

/// The placeholder a redacted value is replaced with.
pub const REDACTED: &str = "[redacted]";

/// FNV-1a 64 — a tiny, dependency-free, deterministic content hash of a payload's
/// serialization (the `payload_hash` column). Not security-sensitive; the same
/// algorithm the flowrunner guest uses for its trace id, so the two never diverge.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Recursively redact secret-bearing values in `v`, in place. A value whose KEY
/// name contains a secret fragment (case-insensitive) is replaced wholesale — even
/// an object/array, so we never recurse into a `credentials` subtree — and every
/// remaining string leaf is checked against the value-shape prefixes. Returns
/// whether anything was redacted (the `redacted` flag).
pub fn scrub(v: &mut Value) -> bool {
    match v {
        Value::Object(map) => {
            let mut changed = false;
            for (k, val) in map.iter_mut() {
                if key_is_secret(k) {
                    *val = Value::String(REDACTED.to_string());
                    changed = true;
                } else {
                    changed |= scrub(val);
                }
            }
            changed
        }
        Value::Array(items) => {
            let mut changed = false;
            for it in items.iter_mut() {
                changed |= scrub(it);
            }
            changed
        }
        Value::String(s) if value_is_secret(s) => {
            *v = Value::String(REDACTED.to_string());
            true
        }
        _ => false,
    }
}

fn key_is_secret(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SECRET_KEY_FRAGMENTS.iter().any(|f| lower.contains(f))
}

fn value_is_secret(s: &str) -> bool {
    SECRET_VALUE_PREFIXES.iter().any(|p| s.starts_with(p))
}

/// The first [`PREVIEW_CHARS`] chars of `s`, on a char boundary.
fn head(s: &str) -> String {
    s.chars().take(PREVIEW_CHARS).collect()
}

/// The capture columns [`derive`] fills for one `node_runs` row. A `None` payload
/// field is a SQL NULL (capture off / preview / oversized); the `*_json` strings
/// are already serialized, ready for a `text`→`jsonb` bind.
#[derive(Debug, Clone, PartialEq)]
pub struct Captured {
    /// The stored `output_json`, or None for a NULL column.
    pub output_json: Option<String>,
    /// The stored `input_json`, or None for a NULL column.
    pub input_json: Option<String>,
    /// The scrubbed output preview head, or None (`off`).
    pub preview_head: Option<String>,
    /// The full serialized output byte length, or None (`off`).
    pub payload_size: Option<i64>,
    /// The output content hash (16 hex chars), or None (`off`).
    pub payload_hash: Option<String>,
    /// The effective `capture_mode` literal (`full`/`scrubbed`/`preview`/`off`).
    pub capture_mode: &'static str,
    /// Whether the STORED payloads were scrubbed (`scrubbed` mode).
    pub redacted: bool,
}

impl Captured {
    /// The `off` row: nothing captured — not even the preview/size/hash.
    fn off() -> Captured {
        Captured {
            output_json: None,
            input_json: None,
            preview_head: None,
            payload_size: None,
            payload_hash: None,
            capture_mode: CaptureMode::Off.as_str(),
            redacted: false,
        }
    }
}

/// Fold a flow's [`Capture`] policy over one node execution's `(output, input)`
/// into the `node_runs` capture columns. See the module docs for the per-mode
/// semantics; `preview_head`/`payload_size`/`payload_hash` always describe the
/// OUTPUT emission (the reconstruction-relevant payload).
pub fn derive(policy: &Capture, output: &Value, input: &Value) -> Captured {
    // `off` captures nothing — the earliest exit, before any serialization work.
    if policy.mode == CaptureMode::Off {
        return Captured::off();
    }

    // The faithful output serialization drives size/hash and the size threshold.
    let raw = output.to_string();
    let payload_size = raw.len() as i64;
    let payload_hash = format!("{:016x}", fnv1a64(raw.as_bytes()));

    // The preview is ALWAYS scrubbed + truncated, so the inspector never surfaces
    // a raw secret regardless of mode (the `full`-mode preview-only scrub).
    let mut preview_val = output.clone();
    scrub(&mut preview_val);
    let preview_head = head(&preview_val.to_string());

    // The size threshold forces preview-only storage in ANY mode (5.10 will move
    // the bytes to the object store; v0 truncates to preview in Postgres).
    let oversized = raw.len() as u64 > policy.max_bytes;
    let effective = if oversized {
        CaptureMode::Preview
    } else {
        policy.mode
    };

    let (output_json, input_json, redacted) = match effective {
        // preview: head + size + hash only; payloads NULL → reconstruct CaptureOff.
        CaptureMode::Preview => (None, None, false),
        // scrubbed: the STORED payloads are scrubbed (a replay replays them).
        CaptureMode::Scrubbed => {
            let mut out = output.clone();
            scrub(&mut out);
            let mut inp = input.clone();
            scrub(&mut inp);
            (Some(out.to_string()), Some(inp.to_string()), true)
        }
        // full: payloads stored faithfully (Off returned above; Preview/Scrubbed
        // handled — this arm is Full).
        _ => (Some(raw), Some(input.to_string()), false),
    };

    Captured {
        output_json,
        input_json,
        preview_head: Some(preview_head),
        payload_size: Some(payload_size),
        payload_hash: Some(payload_hash),
        capture_mode: effective.as_str(),
        redacted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn full() -> Capture {
        Capture::default()
    }
    fn with_mode(mode: CaptureMode) -> Capture {
        Capture {
            mode,
            ..Capture::default()
        }
    }

    // ---- scrub ------------------------------------------------------------

    #[test]
    fn scrub_redacts_secret_keys_case_insensitively() {
        let mut v = json!({
            "PassWord": "hunter2",
            "api_key": "abc",
            "authorization": "Basic zzz",
            "user": "alice",
            "count": 5,
        });
        assert!(scrub(&mut v));
        assert_eq!(v["PassWord"], json!(REDACTED));
        assert_eq!(v["api_key"], json!(REDACTED));
        assert_eq!(v["authorization"], json!(REDACTED));
        // Non-secret keys are untouched.
        assert_eq!(v["user"], json!("alice"));
        assert_eq!(v["count"], json!(5));
    }

    #[test]
    fn scrub_redacts_a_whole_secret_subtree_without_recursing() {
        // A secret KEY's value is redacted wholesale even when it is an object —
        // we never leave a nested secret behind by recursing into it.
        let mut v = json!({ "credentials": { "token": "t", "nested": { "deep": "x" } } });
        assert!(scrub(&mut v));
        assert_eq!(v["credentials"], json!(REDACTED));
    }

    #[test]
    fn scrub_redacts_secret_value_shapes_under_innocent_keys() {
        let mut v = json!({
            "header": "Bearer eyJabc.def.ghi",
            "cert": "-----BEGIN PRIVATE KEY-----\nAAA",
            "aws": "AKIAIOSFODNN7EXAMPLE",
            "plain": "just text",
        });
        assert!(scrub(&mut v));
        assert_eq!(v["header"], json!(REDACTED));
        assert_eq!(v["cert"], json!(REDACTED));
        assert_eq!(v["aws"], json!(REDACTED));
        assert_eq!(v["plain"], json!("just text"));
    }

    #[test]
    fn scrub_recurses_arrays_and_reports_no_change_when_clean() {
        let mut v = json!({ "rows": [{ "token": "a" }, { "ok": 1 }] });
        assert!(scrub(&mut v));
        assert_eq!(v["rows"][0]["token"], json!(REDACTED));
        assert_eq!(v["rows"][1]["ok"], json!(1));

        let mut clean = json!({ "a": 1, "b": ["x", { "c": true }] });
        assert!(!scrub(&mut clean), "a clean payload reports no redaction");
    }

    // ---- derive: per mode -------------------------------------------------

    #[test]
    fn full_stores_faithfully_but_scrubs_only_the_preview() {
        let out = json!({ "password": "hunter2", "value": "ok" });
        let inp = json!({ "password": "hunter2" });
        let c = derive(&full(), &out, &inp);
        assert_eq!(c.capture_mode, "full");
        assert!(!c.redacted);
        // Stored payloads are exact (replayable).
        assert_eq!(c.output_json.as_deref(), Some(out.to_string().as_str()));
        assert_eq!(c.input_json.as_deref(), Some(inp.to_string().as_str()));
        assert!(c.output_json.as_deref().unwrap().contains("hunter2"));
        // The preview is scrubbed — the inspector never shows the secret.
        let preview = c.preview_head.unwrap();
        assert!(!preview.contains("hunter2"));
        assert!(preview.contains(REDACTED));
        // Size/hash describe the faithful output.
        assert_eq!(c.payload_size, Some(out.to_string().len() as i64));
        assert_eq!(
            c.payload_hash.as_deref(),
            Some(format!("{:016x}", fnv1a64(out.to_string().as_bytes())).as_str())
        );
    }

    #[test]
    fn scrubbed_scrubs_the_stored_payloads_and_sets_redacted() {
        let out = json!({ "token": "sekret-OUT", "value": "ok" });
        let inp = json!({ "api_key": "sekret-IN" });
        let c = derive(&with_mode(CaptureMode::Scrubbed), &out, &inp);
        assert_eq!(c.capture_mode, "scrubbed");
        assert!(c.redacted);
        // The raw secrets appear NOWHERE in the stored payloads or the preview.
        let stored = format!(
            "{}{}{}",
            c.output_json.as_deref().unwrap(),
            c.input_json.as_deref().unwrap(),
            c.preview_head.as_deref().unwrap(),
        );
        assert!(!stored.contains("sekret-OUT"));
        assert!(!stored.contains("sekret-IN"));
        assert!(c.output_json.as_deref().unwrap().contains(REDACTED));
        // A replay still has a payload to fold (not NULL).
        assert!(c.output_json.is_some());
    }

    #[test]
    fn preview_nulls_payloads_but_keeps_head_size_hash() {
        let out = json!({ "value": "ok", "n": 3 });
        let c = derive(&with_mode(CaptureMode::Preview), &out, &json!({ "in": 1 }));
        assert_eq!(c.capture_mode, "preview");
        assert!(!c.redacted);
        assert_eq!(c.output_json, None);
        assert_eq!(c.input_json, None);
        assert!(c.preview_head.is_some());
        assert_eq!(c.payload_size, Some(out.to_string().len() as i64));
        assert!(c.payload_hash.is_some());
    }

    #[test]
    fn off_captures_nothing() {
        let c = derive(&with_mode(CaptureMode::Off), &json!({ "a": 1 }), &json!({}));
        assert_eq!(c.capture_mode, "off");
        assert!(!c.redacted);
        assert_eq!(c.output_json, None);
        assert_eq!(c.input_json, None);
        assert_eq!(c.preview_head, None);
        assert_eq!(c.payload_size, None);
        assert_eq!(c.payload_hash, None);
    }

    // ---- derive: size threshold ------------------------------------------

    #[test]
    fn oversized_payload_is_stored_preview_only_in_any_mode() {
        // A `full`-policy payload over the threshold is truncated to preview-only,
        // recorded as capture_mode='preview' with the FULL size/hash retained.
        let big = "x".repeat(200);
        let out = json!({ "blob": big });
        let raw_len = out.to_string().len();
        let policy = Capture {
            mode: CaptureMode::Full,
            max_bytes: 64,
        };
        let c = derive(&policy, &out, &json!({ "in": 1 }));
        assert_eq!(c.capture_mode, "preview");
        assert_eq!(c.output_json, None, "oversized payload not stored");
        assert_eq!(c.input_json, None);
        assert_eq!(c.payload_size, Some(raw_len as i64), "full size retained");
        assert_eq!(
            c.payload_hash.as_deref(),
            Some(format!("{:016x}", fnv1a64(out.to_string().as_bytes())).as_str())
        );

        // At/under the threshold, the same `full` policy stores faithfully.
        let small = json!({ "n": 1 });
        let under = Capture {
            mode: CaptureMode::Full,
            max_bytes: 1024,
        };
        assert_eq!(derive(&under, &small, &json!({})).capture_mode, "full");
    }

    #[test]
    fn preview_head_is_truncated_to_the_char_budget() {
        let out = json!("z".repeat(1000));
        let c = derive(&full(), &out, &json!({}));
        let preview = c.preview_head.unwrap();
        assert_eq!(preview.chars().count(), PREVIEW_CHARS);
    }
}
