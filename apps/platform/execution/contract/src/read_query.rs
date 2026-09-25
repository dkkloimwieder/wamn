//! The one query-string encoding of a read request item.
//!
//! A read route is an HTTP GET, so its one request item travels in the query
//! string (`docs/plan/http-reads.md` section 4.1). Each top-level member is one
//! parameter. Its value is the canonical JSON text of the member, strings
//! included, so a decoder needs no schema. Parameters stand in byte order of
//! their names, and every byte outside the RFC 3986 unreserved set is escaped
//! as `%XX` with uppercase hex. Equal items therefore give equal URLs, which a
//! cache keys on.

use std::fmt::Write as _;

use serde_json::{Map, Value};

use crate::canonical_json_bytes;

/// Why a query string is not the canonical encoding of a read item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadQueryError {
    /// A parameter has no `=`, or an escape is not `%` and two uppercase hex digits.
    Malformed,
    /// A name is not UTF-8 after unescaping.
    NotUtf8,
    /// A value is not JSON.
    NotJson,
    /// The query string decodes, but encoding its item gives other bytes: a
    /// parameter repeats, stands out of order, or is escaped or spelled
    /// differently.
    NotCanonical,
}

impl std::fmt::Display for ReadQueryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Malformed => "the query string is malformed",
            Self::NotUtf8 => "a query parameter name is not UTF-8",
            Self::NotJson => "a query parameter value is not JSON",
            Self::NotCanonical => "the query string is not the canonical encoding of its item",
        })
    }
}

impl std::error::Error for ReadQueryError {}

/// Encode one read item as its canonical query string, without the leading `?`.
///
/// An item with no members gives the empty string.
pub fn encode_read_query(item: &Map<String, Value>) -> String {
    let mut query = String::new();
    // A serde_json map is a BTreeMap, so its keys iterate in byte order.
    for (name, value) in item {
        if !query.is_empty() {
            query.push('&');
        }
        escape(name.as_bytes(), &mut query);
        query.push('=');
        escape(&canonical_json_bytes(value), &mut query);
    }
    query
}

/// Decode a query string that must be the canonical encoding of one read item.
///
/// # Errors
///
/// Returns [`ReadQueryError`] when the query string is not exactly what
/// [`encode_read_query`] writes for the item it decodes to.
pub fn decode_read_query(query: &str) -> Result<Map<String, Value>, ReadQueryError> {
    let mut item = Map::new();
    if !query.is_empty() {
        for parameter in query.split('&') {
            let (name, value) = parameter.split_once('=').ok_or(ReadQueryError::Malformed)?;
            let name = String::from_utf8(unescape(name)?).map_err(|_| ReadQueryError::NotUtf8)?;
            let value = serde_json::from_slice::<Value>(&unescape(value)?)
                .map_err(|_| ReadQueryError::NotJson)?;
            item.insert(name, value);
        }
    }
    if encode_read_query(&item) != query {
        return Err(ReadQueryError::NotCanonical);
    }
    Ok(item)
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

fn unescape(text: &str) -> Result<Vec<u8>, ReadQueryError> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        rest = tail;
        if byte == b'%' {
            let [high, low, tail @ ..] = rest else {
                return Err(ReadQueryError::Malformed);
            };
            bytes.push((hex(*high)? << 4) | hex(*low)?);
            rest = tail;
        } else {
            bytes.push(byte);
        }
    }
    Ok(bytes)
}

fn hex(digit: u8) -> Result<u8, ReadQueryError> {
    match digit {
        b'0'..=b'9' => Ok(digit - b'0'),
        b'A'..=b'F' => Ok(digit - b'A' + 10),
        _ => Err(ReadQueryError::Malformed),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value};

    use super::{ReadQueryError, decode_read_query, encode_read_query};

    /// The vectors that the TypeScript encoder in `web/runtime` also reads.
    const VECTORS: &str = include_str!("../read-query-vectors.json");

    #[test]
    fn every_vector_encodes_to_its_query_and_decodes_back() {
        let vectors: Vec<Value> = serde_json::from_str(VECTORS).expect("vector file is JSON");
        assert!(!vectors.is_empty());
        for vector in vectors {
            let item: Map<String, Value> =
                serde_json::from_value(vector["item"].clone()).expect("vector item is an object");
            let query = vector["query"].as_str().expect("vector query is a string");
            assert_eq!(encode_read_query(&item), query, "{vector}");
            assert_eq!(decode_read_query(query).as_ref(), Ok(&item), "{vector}");
        }
    }

    #[test]
    fn a_query_that_is_not_the_canonical_encoding_is_refused() {
        for (query, error) in [
            ("b=1&a=2", ReadQueryError::NotCanonical),
            ("a=1&a=2", ReadQueryError::NotCanonical),
            ("a=%20%31", ReadQueryError::NotCanonical),
            ("a=%31", ReadQueryError::NotCanonical),
            ("a=%2", ReadQueryError::Malformed),
            ("a=%2c", ReadQueryError::Malformed),
            ("a", ReadQueryError::Malformed),
            ("a=abc", ReadQueryError::NotJson),
            ("a=%FF", ReadQueryError::NotJson),
            ("%FF=1", ReadQueryError::NotUtf8),
        ] {
            assert_eq!(decode_read_query(query), Err(error), "{query}");
        }
    }
}
