//! Shared execution contract vocabulary.
//!
//! MVP outcome: crash floor · M0 execution · flow composition.
//!
//! What outlived the flow-language retirement (wamn-0h0g.26.5) is the
//! vocabulary other bounded contexts still speak:
//!
//! - **canonical JSON** — [`canonical_json_bytes`] / [`canonical_json_sha256`],
//!   the byte-exact identity every durable boundary binds;
//! - **node contract** — [`node_contract`], the emission, failure and
//!   connection-requirement vocabulary plus portable HTTP request-target
//!   normalization, all that outlived the standard-node registry
//!   (wamn-0h0g.26.14);
//! - **ports** — [`MAIN_PORT`] / [`ERROR_PORT`] / [`EntryKind`], persisted names
//!   the walk and the exposure row both read;
//! - **cases** — [`TestSetCase`] and [`Expect`], the publish gate's bounded test
//!   contract;
//! - **failure** — [`WiringFailureKind`], the frozen `failure-code` literals.

mod expect;
pub mod node_contract;
mod ports;
mod status;
mod test_set;

use std::fmt::Write as _;

use serde_json::Value;
use sha2::{Digest as _, Sha256};

pub use expect::{Expect, ExpectError, ExpectedOutcome};
pub use node_contract::{
    CanonicalHttpTarget, ConnectionRequirement, PortableHttpTargetError,
    normalize_portable_http_target,
};
pub use ports::{ERROR_PORT, EntryKind, MAIN_PORT};
pub use status::WiringFailureKind;
pub use test_set::{
    MAX_TEST_SET_CASES, TestSetCase, TestSetCasesError, TestSetCasesErrorKind, validate_cases,
};

/// `sha256:<hex>` over the canonical representation of arbitrary JSON.
///
/// This is the stable identity used when a durable boundary must compare a
/// replayed JSON body with the outcome that previously committed.
pub fn canonical_json_sha256(value: &Value) -> String {
    let digest = Sha256::digest(canonical_json_bytes(value));
    let mut hash = String::with_capacity("sha256:".len() + digest.len() * 2);
    hash.push_str("sha256:");
    for byte in digest {
        write!(&mut hash, "{byte:02x}").expect("writing to String cannot fail");
    }
    hash
}

/// Canonical bytes for an arbitrary JSON value.
///
/// Determinism comes from `serde_json` itself: without the `preserve_order`
/// feature a `serde_json::Map` is a `BTreeMap`, so object keys always serialize
/// in one order regardless of insertion order, and `float_roundtrip` keeps a
/// parsed `f64` exact so re-serializing reproduces the same bytes. The crate
/// manifest pins both conditions and
/// `canonical_json_hash_ignores_object_insertion_order` guards the first.
///
/// wamn-0h0g.26.5 replaced a hand-rolled RFC 8785 canonicalizer with this:
/// every model that enters a digest is already `BTreeMap`/`BTreeSet`-shaped
/// with `deny_unknown_fields` and a pinned `format_version`, and no non-Rust
/// verifier of any digest exists in this workspace, so the extra spelling rules
/// bought nothing the type layer was not already buying.
pub fn canonical_json_bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("a serde_json::Value always serializes")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{canonical_json_bytes, canonical_json_sha256};

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
        let left = json!({"z": -0.0, "large": 1e30, "small": 0.000001, "nested": {"b": 2, "a": 1}});
        let reordered =
            json!({"nested": {"a": 1, "b": 2}, "small": 0.000001, "large": 1e30, "z": -0.0});
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
