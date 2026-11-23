//! Canonical JSON bytes and SHA-256 identities.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// A SHA-256 identity, serialized as `sha256:<64 lowercase hex digits>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest([u8; 32]);

/// A refusal to parse a canonical SHA-256 identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseDigestError;

impl fmt::Display for ParseDigestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected sha256:<64 lowercase hex digits>")
    }
}

impl std::error::Error for ParseDigestError {}

impl FromStr for Digest {
    type Err = ParseDigestError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let hex = value.strip_prefix("sha256:").ok_or(ParseDigestError)?;
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(ParseDigestError);
        }
        let mut bytes = [0; 32];
        for (byte, pair) in bytes.iter_mut().zip(hex.as_bytes().as_chunks::<2>().0) {
            let digit = |value: u8| {
                if value <= b'9' {
                    value - b'0'
                } else {
                    value - b'a' + 10
                }
            };
            *byte = digit(pair[0]) * 16 + digit(pair[1]);
        }
        Ok(Self(bytes))
    }
}

impl TryFrom<String> for Digest {
    type Error = ParseDigestError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<[u8; 32]> for Digest {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl From<Digest> for String {
    fn from(digest: Digest) -> Self {
        digest.to_string()
    }
}

impl From<Digest> for Value {
    fn from(digest: Digest) -> Self {
        Self::String(digest.to_string())
    }
}

impl Digest {
    /// The raw SHA-256 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hexadecimal digits for an OCI tag or an object path.
    pub fn hex(&self) -> String {
        use fmt::Write as _;
        let mut hex = String::with_capacity(64);
        for byte in self.0 {
            write!(hex, "{byte:02x}").expect("writing to String cannot fail");
        }
        hex
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("sha256:")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// `sha256:<hex>` over the canonical representation of arbitrary JSON.
///
/// This is the stable identity used when a durable boundary must compare a
/// replayed JSON body with the outcome that previously committed.
pub fn canonical_json_sha256(value: &Value) -> Digest {
    bytes_sha256(&canonical_json_bytes(value))
}

/// Canonical bytes for an arbitrary JSON value.
///
/// Object keys serialize in sorted order at every depth, and this function
/// sorts them itself. Canonical bytes are record identity, so they must not
/// depend on the `serde_json` features that a build unifies: a crate that
/// turns on `preserve_order` (jsonata-core does) makes a `serde_json::Map`
/// keep insertion order in every crate of that build (wamn-g1jd).
/// `float_roundtrip` keeps a parsed `f64` exact, so
/// re-serializing reproduces the same bytes.
///
/// wamn-0h0g.26.5 replaced a hand-rolled RFC 8785 canonicalizer with this:
/// every model that enters a digest is already `BTreeMap`/`BTreeSet`-shaped
/// with `deny_unknown_fields` and a pinned `format_version`, and no non-Rust
/// verifier of any digest exists in this workspace, so the extra spelling rules
/// bought nothing the type layer was not already buying.
pub fn canonical_json_bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(&SortedKeys(value)).expect("a serde_json::Value always serializes")
}

/// A `Value` that serializes its object keys in sorted order at every depth.
struct SortedKeys<'a>(&'a Value);

impl serde::Serialize for SortedKeys<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self.0 {
            Value::Object(map) => {
                let mut entries: Vec<_> = map.iter().collect();
                entries.sort_unstable_by_key(|(key, _)| *key);
                let mut object = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    object.serialize_entry(key, &SortedKeys(value))?;
                }
                object.end()
            }
            Value::Array(items) => {
                let mut array = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    array.serialize_element(&SortedKeys(item))?;
                }
                array.end()
            }
            scalar => scalar.serialize(serializer),
        }
    }
}

/// `sha256:<lowercase hex>` over exact bytes, with no JSON step.
///
/// This is the one spelling of a raw-byte digest: file, bundle, component and
/// build-record digests all use it. [`canonical_json_sha256`] is the same
/// spelling over canonical JSON bytes.
///
pub fn bytes_sha256(bytes: &[u8]) -> Digest {
    format_sha256(&sha256(bytes))
}

/// SHA-256 over exact bytes.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

/// Incremental SHA-256 for framed inputs.
#[derive(Debug, Clone, Default)]
pub struct Hasher(Sha256);

impl Hasher {
    /// Start a SHA-256 input stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append bytes to the input stream.
    pub fn update(&mut self, bytes: impl AsRef<[u8]>) {
        self.0.update(bytes);
    }

    /// Finish the stream and return its SHA-256 result.
    pub fn finalize(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

/// Format a SHA-256 result as `sha256:<lowercase hex>`.
pub fn format_sha256(digest: &[u8; 32]) -> Digest {
    Digest(*digest)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::bytes_sha256;
    use super::{canonical_json_bytes, canonical_json_sha256};

    #[test]
    fn parse_rule() {
        for (value, valid) in [
            (format!("sha256:{}", "0".repeat(64)), true),
            (format!("sha256:{}", "0123456789abcdef".repeat(4)), true),
            (String::new(), false),
            ("0".repeat(64), false),
            (format!("sha256:{}", "a".repeat(63)), false),
            (format!("sha256:{}", "a".repeat(65)), false),
            (format!("sha256:{}", "A".repeat(64)), false),
            (format!("sha256:{}", "g".repeat(64)), false),
            (format!("sha256:{}", "é".repeat(32)), false),
            (format!("sha512:{}", "a".repeat(64)), false),
        ] {
            let parsed = value.parse::<super::Digest>();
            assert_eq!(parsed.is_ok(), valid);
            if let Ok(digest) = parsed {
                assert_eq!(digest.to_string(), value);
                assert_eq!(
                    serde_json::from_value::<super::Digest>(serde_json::to_value(digest).unwrap())
                        .unwrap(),
                    digest
                );
            }
        }
    }

    #[test]
    fn raw_byte_digests_match_fixed_sha256_vectors() {
        let binary: Vec<u8> = (0..=255).collect();

        assert_eq!(
            bytes_sha256(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                .parse::<super::Digest>()
                .unwrap()
        );
        assert_eq!(
            bytes_sha256(&binary),
            "sha256:40aff2e9d2d8922e47afd4648e6967497158785fbd1da870e7110266bf944880"
                .parse::<super::Digest>()
                .unwrap()
        );
        assert_eq!(
            bytes_sha256("wamn ✓\n".as_bytes()),
            "sha256:b6f49645cc50cf7d007a270a1ee823cc1c8ccd47e3ee4fd281ed6800409229be"
                .parse::<super::Digest>()
                .unwrap()
        );
    }

    #[test]
    fn canonical_json_hash_is_the_raw_digest_of_canonical_bytes() {
        let value = json!({"b": [1, "two"], "a": null});

        assert_eq!(
            canonical_json_sha256(&value),
            bytes_sha256(&canonical_json_bytes(&value))
        );
    }

    #[test]
    fn canonical_json_hash_ignores_object_insertion_order() {
        let left = canonical_json_sha256(&json!({"a": 1, "b": 2}));
        let reordered = canonical_json_sha256(&json!({"b": 2, "a": 1}));
        let changed = canonical_json_sha256(&json!({"a": 9, "b": 2}));

        assert_eq!(left, reordered);
        assert_ne!(left, changed);
    }

    #[test]
    fn durable_identity_bytes_cover_ordering_and_numeric_edges() {
        let left =
            json!({"z": -0.0, "large": 1e30, "small": 0.000_001, "nested": {"b": 2, "a": 1}});
        let reordered =
            json!({"nested": {"a": 1, "b": 2}, "small": 0.000_001, "large": 1e30, "z": -0.0});
        let bytes = canonical_json_bytes(&left);

        assert_eq!(bytes, canonical_json_bytes(&reordered));
        assert_eq!(
            canonical_json_sha256(&left),
            canonical_json_sha256(&reordered)
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::from_slice::<serde_json::Value>(&canonical_json_bytes(&reordered)).unwrap()
        );
    }
}
