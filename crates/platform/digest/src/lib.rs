//! Canonical JSON bytes and SHA-256 identities.

use std::fmt::Write as _;

use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// `sha256:<hex>` over the canonical representation of arbitrary JSON.
///
/// This is the stable identity used when a durable boundary must compare a
/// replayed JSON body with the outcome that previously committed.
pub fn canonical_json_sha256(value: &Value) -> String {
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
pub fn bytes_sha256(bytes: &[u8]) -> String {
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
pub fn format_sha256(digest: &[u8; 32]) -> String {
    let mut hash = String::with_capacity("sha256:".len() + digest.len() * 2);
    hash.push_str("sha256:");
    for byte in digest {
        write!(&mut hash, "{byte:02x}").expect("writing to String cannot fail");
    }
    hash
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::bytes_sha256;
    use super::{canonical_json_bytes, canonical_json_sha256};

    #[test]
    fn raw_byte_digests_match_fixed_sha256_vectors() {
        let binary: Vec<u8> = (0..=255).collect();

        assert_eq!(
            bytes_sha256(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            bytes_sha256(&binary),
            "sha256:40aff2e9d2d8922e47afd4648e6967497158785fbd1da870e7110266bf944880"
        );
        assert_eq!(
            bytes_sha256("wamn ✓\n".as_bytes()),
            "sha256:b6f49645cc50cf7d007a270a1ee823cc1c8ccd47e3ee4fd281ed6800409229be"
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
        assert!(left.starts_with("sha256:"));
        assert_eq!(left.len(), "sha256:".len() + 64);
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
