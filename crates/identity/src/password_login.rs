//! Password login records and renewal authentication.

use std::fmt;

use wamn_identity_protocol::PrincipalId;
use zeroize::Zeroizing;

/// Maximum login duration from password authentication, in seconds.
pub const ABSOLUTE_LIFETIME: i64 = 8 * 60 * 60;
/// Maximum time between successful renewals, in seconds.
pub const INACTIVITY_LIFETIME: i64 = 30 * 60;

/// A stored login's immutable identity and absolute deadline.
#[derive(Debug, Clone)]
pub struct Login {
    pub id: String,
    pub principal: PrincipalId,
    pub authenticated_at: i64,
    pub expires_at: i64,
}

/// One replacement credential, returned only after its transaction commits.
pub struct Renewal {
    pub login: Login,
    secret: Zeroizing<String>,
}
impl Renewal {
    /// Return the new credential and its login for delivery after commit.
    pub fn new(login: Login, secret: Zeroizing<String>) -> Self {
        Self { login, secret }
    }

    /// The opaque credential for private delivery to the authenticated caller.
    pub fn secret(&self) -> &str {
        &self.secret
    }
}
impl fmt::Debug for Renewal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Renewal")
            .field("login", &self.login)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}
