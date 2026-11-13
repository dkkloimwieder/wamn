//! Canonical URL query encoding for read requests.

use serde_json::{Map, Value};
use std::fmt::Write as _;
use wamn_digest::canonical_json_bytes;

/// Encode one read request element as its canonical query string, without the leading
/// `?`.
///
/// A request element with no members gives the empty string.
pub fn encode_read_query(request_element: &Map<String, Value>) -> String {
    let mut query = String::new();
    // Sort the names here: with `preserve_order` a serde_json map keeps
    // insertion order (wamn-g1jd).
    let mut members: Vec<_> = request_element.iter().collect();
    members.sort_unstable_by_key(|(name, _)| *name);
    for (name, value) in members {
        if !query.is_empty() {
            query.push('&');
        }
        escape(name.as_bytes(), &mut query);
        query.push('=');
        escape(&canonical_json_bytes(value), &mut query);
    }
    query
}

fn escape(bytes: &[u8], out: &mut String) {
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            write!(out, "%{byte:02X}").expect("writing to String cannot fail");
        }
    }
}
