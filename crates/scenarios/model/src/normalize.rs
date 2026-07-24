//! Volatile-field normalization (11.3): the rules a record-and-replay case
//! applies SYMMETRICALLY to the expected value and the captured node output
//! before a node-output assertion compares them, so a pinned run stays green
//! across re-runs whose only difference is a server-minted id or timestamp.
//!
//! Two knobs, both pure (`serde_json` only, guest-compilable, NO regex):
//! - `ignore-paths` — a list of RFC-6901 JSON pointers dropped from BOTH sides
//!   (a field the case does not want to assert on at all).
//! - `canonicalize` — replace every UUID-shaped and RFC-3339-`Z` timestamp-shaped
//!   STRING leaf with a fixed placeholder (`[uuid]` / `[timestamp]`), so two runs
//!   that differ only in a minted id/timestamp collapse to the same value.
//!
//! [`normalize`] is applied to expected and actual identically by
//! [`evaluate`](crate::evaluate), so a same-shaped volatile value matches while a
//! REAL field difference survives (see the 11.3 record-and-replay round-trip).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The normalization rules a [`TestCase`](crate::TestCase) carries. Absent on a
/// case ⇒ no normalization (the 11.4 behavior); both fields default off, so an
/// empty `{}` is also a no-op.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Normalize {
    /// RFC-6901 JSON pointers (e.g. `/meta/run-id`) removed from BOTH the
    /// expected and the captured value before comparison. A pointer that does
    /// not resolve is silently ignored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_paths: Vec<String>,
    /// When true, collapse UUID-shaped and RFC-3339-`Z` timestamp-shaped string
    /// leaves to a placeholder so a minted id/timestamp does not fail replay.
    #[serde(default, skip_serializing_if = "is_false")]
    pub canonicalize: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The placeholder a UUID-shaped leaf canonicalizes to.
const UUID_PLACEHOLDER: &str = "[uuid]";
/// The placeholder an RFC-3339-`Z` timestamp-shaped leaf canonicalizes to.
const TIMESTAMP_PLACEHOLDER: &str = "[timestamp]";

/// Apply `rules` to a clone of `value`: drop each `ignore-paths` pointer, then
/// (if `canonicalize`) collapse volatile-shaped string leaves. Pure — the input
/// is untouched.
pub fn normalize(value: &Value, rules: &Normalize) -> Value {
    let mut v = value.clone();
    for pointer in &rules.ignore_paths {
        remove_pointer(&mut v, pointer);
    }
    if rules.canonicalize {
        canonicalize(&mut v);
    }
    v
}

/// Remove the value at an RFC-6901 `pointer` from `v` in place. Splits the
/// pointer into its parent and last token, resolves the parent with
/// [`Value::pointer_mut`], and removes the token (an object key or an array
/// index). A malformed/unresolved pointer is a no-op.
fn remove_pointer(v: &mut Value, pointer: &str) {
    // RFC-6901: a pointer is empty (the whole document) or starts with '/'. We
    // never drop the whole document, and a token-less pointer has no parent.
    let Some((parent_ptr, last)) = pointer.rsplit_once('/') else {
        return;
    };
    let token = unescape_token(last);
    let Some(parent) = v.pointer_mut(parent_ptr) else {
        return;
    };
    match parent {
        Value::Object(map) => {
            map.remove(&token);
        }
        Value::Array(items) => {
            if let Ok(idx) = token.parse::<usize>()
                && idx < items.len()
            {
                items.remove(idx);
            }
        }
        _ => {}
    }
}

/// RFC-6901 token unescape: `~1` → `/`, `~0` → `~` (in that order).
fn unescape_token(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

/// Recursively replace volatile-shaped string leaves with a placeholder.
fn canonicalize(v: &mut Value) {
    match v {
        Value::String(s) => {
            if is_uuid(s) {
                *v = Value::String(UUID_PLACEHOLDER.to_string());
            } else if is_rfc3339_z(s) {
                *v = Value::String(TIMESTAMP_PLACEHOLDER.to_string());
            }
        }
        Value::Array(items) => items.iter_mut().for_each(canonicalize),
        Value::Object(map) => map.values_mut().for_each(canonicalize),
        _ => {}
    }
}

/// A canonical UUID string: 36 chars, hyphens at 8/13/18/23, every other char a
/// hex digit. Char-position checks only — no regex (the capture.rs precedent).
fn is_uuid(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    s.bytes().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => b == b'-',
        _ => b.is_ascii_hexdigit(),
    })
}

/// A NARROW RFC-3339 UTC timestamp: `YYYY-MM-DDTHH:MM:SS[.fraction]Z`. The fixed
/// separators are pinned by position, the datetime positions are digits, an
/// optional fractional part is digits, and it ends in `Z`. Deliberately
/// conservative — a bare date (`2026-07-22`) or a non-`Z` offset does NOT match.
fn is_rfc3339_z(s: &str) -> bool {
    let b = s.as_bytes();
    // "2026-07-22T06:59:00Z" is the minimal 20-char shape.
    if b.len() < 20 || b[b.len() - 1] != b'Z' {
        return false;
    }
    // Fixed separators.
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return false;
    }
    // The datetime digit positions.
    for i in [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18] {
        if !b[i].is_ascii_digit() {
            return false;
        }
    }
    // The remainder (between seconds and the trailing 'Z') is an optional
    // fractional part: a '.' followed by digits, nothing else.
    let frac = &b[19..b.len() - 1];
    match frac.split_first() {
        None => true,
        Some((b'.', rest)) => !rest.is_empty() && rest.iter().all(u8::is_ascii_digit),
        Some(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ignore_paths_drops_fields_on_the_named_pointers() {
        let v = json!({"keep": 1, "vol": "x", "meta": {"id": "abc", "ok": true}});
        let rules = Normalize {
            ignore_paths: vec!["/vol".into(), "/meta/id".into()],
            canonicalize: false,
        };
        let out = normalize(&v, &rules);
        assert_eq!(out, json!({"keep": 1, "meta": {"ok": true}}));
        // The input is untouched (pure).
        assert!(v.get("vol").is_some());
    }

    #[test]
    fn ignore_paths_can_drop_an_array_element() {
        let v = json!({"xs": [10, 11, 12]});
        let out = normalize(
            &v,
            &Normalize {
                ignore_paths: vec!["/xs/1".into()],
                canonicalize: false,
            },
        );
        assert_eq!(out, json!({"xs": [10, 12]}));
    }

    #[test]
    fn unresolved_or_malformed_pointer_is_a_noop() {
        let v = json!({"a": 1});
        for p in ["/missing", "/a/b/c", "no-leading-slash", ""] {
            let out = normalize(
                &v,
                &Normalize {
                    ignore_paths: vec![p.into()],
                    canonicalize: false,
                },
            );
            assert_eq!(out, v, "pointer {p:?} must be a no-op");
        }
    }

    #[test]
    fn canonicalize_collapses_uuid_and_timestamp_leaves() {
        let v = json!({
            "run_id": "550e8400-e29b-41d4-a716-446655440000",
            "at": "2026-07-22T06:59:00Z",
            "at_frac": "2026-07-22T06:59:00.123Z",
            "keep": "reject",
            "date": "2026-07-22",
            "n": 5,
        });
        let out = normalize(
            &v,
            &Normalize {
                ignore_paths: vec![],
                canonicalize: true,
            },
        );
        assert_eq!(out["run_id"], json!("[uuid]"));
        assert_eq!(out["at"], json!("[timestamp]"));
        assert_eq!(out["at_frac"], json!("[timestamp]"));
        // A real value, a bare date, and a number are untouched.
        assert_eq!(out["keep"], json!("reject"));
        assert_eq!(out["date"], json!("2026-07-22"));
        assert_eq!(out["n"], json!(5));
    }

    #[test]
    fn uuid_boundary_checks() {
        assert!(is_uuid("550e8400-e29b-41d4-a716-446655440000"));
        // wrong length
        assert!(!is_uuid("550e8400-e29b-41d4-a716-44665544000"));
        // dash in the wrong place
        assert!(!is_uuid("550e8400e-29b-41d4-a716-446655440000"));
        // a non-hex char at a hex position
        assert!(!is_uuid("550e8400-e29b-41d4-a716-4466554400zz"));
    }

    #[test]
    fn timestamp_boundary_checks() {
        assert!(is_rfc3339_z("2026-07-22T06:59:00Z"));
        assert!(is_rfc3339_z("2026-07-22T06:59:00.9Z"));
        // no trailing Z / an offset instead
        assert!(!is_rfc3339_z("2026-07-22T06:59:00+00:00"));
        // a bare date
        assert!(!is_rfc3339_z("2026-07-22"));
        // a trailing dot with no fraction
        assert!(!is_rfc3339_z("2026-07-22T06:59:00.Z"));
        // a letter where a digit belongs
        assert!(!is_rfc3339_z("2026-07-22T06:59:0aZ"));
    }

    #[test]
    fn round_trips_and_defaults_off() {
        // An empty rule set is a no-op and serializes to `{}`.
        let empty = Normalize::default();
        assert_eq!(serde_json::to_value(&empty).unwrap(), json!({}));
        let v = json!({"a": "550e8400-e29b-41d4-a716-446655440000"});
        assert_eq!(normalize(&v, &empty), v);

        // kebab-case wire form round-trips.
        let rules = Normalize {
            ignore_paths: vec!["/meta/id".into()],
            canonicalize: true,
        };
        let wire = json!({"ignore-paths": ["/meta/id"], "canonicalize": true});
        assert_eq!(serde_json::to_value(&rules).unwrap(), wire);
        let back: Normalize = serde_json::from_value(wire).unwrap();
        assert_eq!(back, rules);
    }
}
