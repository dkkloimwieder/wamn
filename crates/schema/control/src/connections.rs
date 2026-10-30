//! Portable connection requirements and environment-owned persistence records.

use sha2::{Digest as _, Sha256};

#[doc(inline)]
pub use wamn_catalog::ComponentConnectionRequirement;

/// The `sha256:<hex>` identity of `bytes`. The package manifest digest and the
/// per-migration digest both use this one copy.
pub(crate) fn prefixed_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity("sha256:".len() + digest.len() * 2);
    out.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut out, "{byte:02x}").expect("writing to a string is infallible");
    }
    out
}
