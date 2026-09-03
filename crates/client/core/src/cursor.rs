//! Opaque cursors.
//!
//! The contract calls a cursor `opaque: true`, and this layer holds it to
//! that: the type carries the string a page returned and nothing else. It does
//! not decode, inspect, or reconstruct one, because a client that understood a
//! cursor's insides would be depending on a shape the platform declared
//! private — and would break the first time keyset ordering changed.

use serde::{Deserialize, Serialize};

/// A page position, exactly as the server issued it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cursor(String);

impl Cursor {
    /// Hold a cursor a page returned.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value to send back, verbatim.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for Cursor {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// EXIT GATE: a cursor round-trips opaquely — what came back is what goes
    /// out, byte for byte, with nothing normalized or re-encoded on the way.
    #[test]
    fn a_cursor_round_trips_byte_for_byte() {
        // Deliberately awkward: base64url-unpadded output that a helpful
        // client might be tempted to pad, trim, or lowercase.
        for issued in [
            "eyJ2IjoxLCJmaWVsZCI6ImNyZWF0ZWRfYXQifQ",
            "AAAA-_9x",
            "",
            "MTIzNDU2Nzg5MA",
        ] {
            let cursor = Cursor::new(issued);
            assert_eq!(cursor.as_str(), issued);
            assert_eq!(cursor.to_string(), issued);

            let encoded = serde_json::to_string(&cursor).expect("serializes");
            let decoded: Cursor = serde_json::from_str(&encoded).expect("deserializes");
            assert_eq!(decoded.as_str(), issued, "round trip changed {issued:?}");
        }
    }

    /// It serializes AS a string, not as an object wrapping one: the wire form
    /// a server issued must be the wire form it receives back.
    #[test]
    fn a_cursor_is_a_bare_string_on_the_wire() {
        let encoded = serde_json::to_string(&Cursor::new("abc")).expect("serializes");
        assert_eq!(encoded, "\"abc\"");
    }
}
