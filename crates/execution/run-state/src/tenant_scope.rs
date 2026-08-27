//! The framed scope digest, and the guest-SQL tenant key derived from it.
//!
//! This module exists so the RUNTIME can compute a tenant key without linking
//! the provisioner (`wamn-0h0g.22.6.7`). After `wamn-0h0g.22.6` the guest's
//! tenant comes from `current_user`, so a resolved guest credential IS a claim
//! about which tenant it may read — and the host has to be able to check that
//! claim before it opens the connection.
//!
//! `wamn-control-provision` DELEGATES here for the App family rather than
//! carrying a second implementation, exactly as it already does for the effect
//! writer. There is one definition of the digest and one of the framing.

use sha2::{Digest as _, Sha256};

/// Scope domain of the guest-SQL (App) family.
pub(crate) const APP_SCOPE_DOMAIN: &[u8] = b"wamn.app.scope.v0.1";

/// Hex width every scope digest is truncated to (160 bits).
pub(crate) const SCOPE_HASH_HEX_LEN: usize = 40;

/// Length-prefix a field into a digest preimage.
///
/// UNAMBIGUOUS BY CONSTRUCTION: an 8-byte big-endian length before every value
/// is what stops `("ab", "c")` and `("a", "bc")` hashing alike, which for role
/// names would be two tenants sharing one login.
pub(crate) fn frame(output: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).expect("identity field length fits u64");
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value);
}

/// Deterministic 160-bit tenant key for one tenant in one database.
///
/// THE SAME VALUE `wamn_authority.tenant_key` COMPUTES IN SQL, and the same one
/// the guest login's name carries. If those disagree by a byte, every governed
/// read by that login refuses.
///
/// The database is part of the preimage because role names are CLUSTER-wide:
/// without it, one tenant in two project-environment databases would derive one
/// key and collide on a single login.
pub fn app_scope_hash(tenant: &str, database: &str) -> String {
    let mut preimage = Vec::new();
    frame(&mut preimage, APP_SCOPE_DOMAIN);
    for (tag, value) in [("tenant", tenant), ("database", database)] {
        frame(&mut preimage, tag.as_bytes());
        frame(&mut preimage, value.as_bytes());
    }
    let digest = hex::encode(Sha256::digest(preimage));
    digest[..SCOPE_HASH_HEX_LEN].to_string()
}

#[cfg(test)]
mod tests {
    use super::app_scope_hash;

    /// The framing is what makes the digest unambiguous, so the case it exists
    /// for is the case that gets asserted.
    #[test]
    fn framing_separates_fields_that_concatenate_alike() {
        assert_ne!(app_scope_hash("ab", "c"), app_scope_hash("a", "bc"));
    }

    /// A VALUE PIN, because the alternatives are tautologies.
    ///
    /// Every other consumer of this digest — the provisioner's `tenant_key`, the
    /// runtime's credential check, the role name — DELEGATES here, so they all
    /// move together and none of them can catch a domain or framing drift. Two
    /// things can: this literal, and the live SQL agreement gate
    /// (`crates/control/provision/tests/tenant_key_live.rs`), which recomputes
    /// the digest inside PostgreSQL from a builder that does NOT delegate.
    #[test]
    fn the_digest_value_is_frozen() {
        assert_eq!(
            app_scope_hash("acme", "wamn-db-acme--billing--dev"),
            "d00c90a7652322315fa96248dcb73b29c3decd54"
        );
    }

    #[test]
    fn the_digest_is_the_forty_hex_scope_convention() {
        let key = app_scope_hash("acme", "wamn-db-acme--billing--dev");
        assert_eq!(key.len(), 40);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
