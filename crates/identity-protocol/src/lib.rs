//! Identity types, credential formats, wire messages and public key documents.

/// Marker every first-party personal access token starts with. A token reads
/// `wamn_pat_<16 hex lookup digits>_<64 hex secret digits>`; the lookup half is
/// stored in the clear as the index key, the whole string only as a digest.
pub const PAT_TOKEN_PREFIX: &str = "wamn_pat_";

mod login;
pub use login::{LoginName, ParseLoginNameError};

mod principal;
pub use principal::{ParseIdentityError, PrincipalId, RoleName};
pub mod pat_prefix;
pub mod secret;
pub mod wire;

mod platform;
pub use platform::{
    ADMIN_ROLE, PLATFORM_PRINCIPAL_PREFIX, PlatformComponent, WAMN_NAMESPACE, WORKFLOW_USER_NAME,
    workflow_user_id,
};

pub mod keys;

/// Credential authority selected by trusted host code.
pub mod authority_class;
pub use authority_class::AuthorityClass;
