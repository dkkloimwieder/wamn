//! The framed identity digest, and the guest-SQL project key derived from it.
//!
//! This module exists so the RUNTIME can compute a project key without linking
//! the provisioner (`wamn-0h0g.22.6.7`). After `wamn-0h0g.22.6` the guest's
//! project comes from `current_user`, so a resolved guest credential IS an identity
//! about which project it may read — and the host has to be able to make sure that
//! identity before it opens the connection.
//!
//! `wamn-control-provision` DELEGATES here for the App family rather than
//! carrying a second implementation. There is one definition of the digest and
//! one of the framing.

use sha2::{Digest as _, Sha256};

/// Target domain of the guest-SQL (App) family.
pub(crate) const APP_IDENTITY_DOMAIN: &[u8] = b"wamn.app.identity.v0.1";

/// Hex width every identity digest is truncated to (160 bits).
pub(crate) const IDENTITY_HASH_HEX_LEN: usize = 40;

/// Length-prefix a field into a digest preimage.
///
/// UNAMBIGUOUS BY CONSTRUCTION: an 8-byte big-endian length before every value
/// is what stops `("ab", "c")` and `("a", "bc")` hashing alike, which for role
/// names would be two projects sharing one login.
pub(crate) fn frame(output: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).expect("identity field length fits u64");
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value);
}

/// Deterministic 160-bit project key for one project in one database.
///
/// THE SAME VALUE `wamn_authority.project_key` COMPUTES IN SQL, and the same one
/// the guest login's name carries. If those disagree by a byte, every governed
/// read by that login refuses.
///
/// The database is part of the preimage because role names are CLUSTER-wide:
/// without it, one project in two project-environment databases would derive one
/// key and collide on a single login.
pub fn app_identity_hash(project_env_key: &str, database: &str) -> String {
    let mut preimage = Vec::new();
    frame(&mut preimage, APP_IDENTITY_DOMAIN);
    for (tag, value) in [("project_env_key", project_env_key), ("database", database)] {
        frame(&mut preimage, tag.as_bytes());
        frame(&mut preimage, value.as_bytes());
    }
    let digest = hex::encode(Sha256::digest(preimage));
    digest[..IDENTITY_HASH_HEX_LEN].to_string()
}

#[cfg(test)]
mod tests {
    use super::app_identity_hash;

    /// The framing is what makes the digest unambiguous, so the case it exists
    /// for is the case that gets asserted.
    #[test]
    fn framing_separates_fields_that_concatenate_alike() {
        assert_ne!(app_identity_hash("ab", "c"), app_identity_hash("a", "bc"));
    }

    #[test]
    fn the_digest_is_the_forty_hex_identity_convention() {
        let key = app_identity_hash("acme", "wamn-db-acme--billing--dev");
        assert_eq!(key.len(), 40);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
