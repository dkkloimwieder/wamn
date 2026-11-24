//! Access-token signing under the caller's login transaction.

use crate::session_token::{IssuedSessionToken, sign_session_token, validate_claims};
use wamn_authentication::identity_error::IdentityError;
use wamn_identity_protocol::records::AccessTokenClaims;

use super::session_keys::sign_message;

/// Sign within the caller's login transaction and optional absolute deadline.
///
/// The caller retains the signing lock through commit and must not deliver a
/// token before that commit succeeds. Supplied JWT claim times are always replaced.
pub async fn sign_session_token_in_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    claims: AccessTokenClaims,
    validation_started_at: i64,
    absolute_expiry: Option<i64>,
    envelope: &wamn_envelope::encryption::Envelope,
) -> Result<IssuedSessionToken, IdentityError> {
    validate_claims(&claims)?;
    let issuer = claims.iss.clone();
    sign_message(transaction, &issuer, envelope, |kid, key| {
        sign_session_token(claims, validation_started_at, absolute_expiry, kid, key)
    })
    .await
}
