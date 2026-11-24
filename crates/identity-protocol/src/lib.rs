//! Identity types, credential formats, wire messages and public key documents.

/// Marker every first-party personal access token starts with. A token reads
/// `wamn_pat_<16 hex lookup digits>_<64 hex secret digits>`; the lookup half is
/// stored in the clear as the index key, the whole string only as a digest.
pub const PAT_TOKEN_PREFIX: &str = "wamn_pat_";

pub mod nats;
pub mod pat_prefix;
pub mod records;
pub mod secret;
pub mod wire;

pub use wamn_principal::{
    PLATFORM_PRINCIPAL_PREFIX, ParseIdentityError, PlatformComponent, PrincipalId, WAMN_NAMESPACE,
    WORKFLOW_USER_NAME, workflow_user_id,
};

/// The built-in role with all served operations.
pub const ADMIN_ROLE: &str = "admin";

pub mod keys;

/// Credential authority selected by trusted host code.
pub mod authority_class;
pub use authority_class::AuthorityClass;
