//! Identity request and response bodies shared by native adapters.

use crate::PrincipalId;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PatResponse<S = String> {
    pub token: S,
    pub token_prefix: crate::pat_prefix::PatPrefix,
    pub principal_id: PrincipalId,
    pub created_at: S,
    pub expires_at: S,
}
impl<S> std::fmt::Debug for PatResponse<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PatResponse").finish_non_exhaustive()
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvitationRequest {
    pub principal_id: PrincipalId,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UserRequest<S = String> {
    pub email: S,
    pub display_name: S,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UserResponse {
    pub principal_id: PrincipalId,
    pub enrolled: bool,
}
