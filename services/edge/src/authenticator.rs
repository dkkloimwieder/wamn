//! The edge's credential mechanism for a protected route.
//!
//! A session token is verified over the key file that the release installs,
//! and its roles map to permissions through the release grants. The box keeps
//! no session state and runs no active-session read, so a verified token's
//! roles hold until the token expires, 930 seconds at most (spec section 4.4).
//! A PAT has no mechanism on the box.

use std::sync::Arc;

use wamn_engine::flow_http_routing::{
    AuthRejection, AuthenticatedCaller, AuthenticationRequest, CredentialKind, RouteAuthenticator,
    RouteCredential, authentication_unavailable, check_csrf, route_credential, unauthorized,
};
use wamn_session::file_keys::FileKeys;
use wamn_session::verifier::SessionVerifier;

use crate::release::EdgeRelease;

/// Sessions over the key file, with permissions from the release grants.
pub struct EdgeAuthenticator {
    verifier: SessionVerifier<FileKeys>,
    release: Arc<EdgeRelease>,
}

impl std::fmt::Debug for EdgeAuthenticator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EdgeAuthenticator")
            .field("bundle_digest", &self.release.bundle_digest())
            .finish_non_exhaustive()
    }
}

impl EdgeAuthenticator {
    /// Authenticate sessions with `verifier` against the grants of `release`.
    pub fn new(verifier: SessionVerifier<FileKeys>, release: Arc<EdgeRelease>) -> Self {
        Self { verifier, release }
    }
}

#[async_trait::async_trait]
impl RouteAuthenticator for EdgeAuthenticator {
    async fn authenticate(
        &self,
        request: AuthenticationRequest<'_>,
    ) -> Result<AuthenticatedCaller, AuthRejection> {
        // An empty cloud authenticator answers a PAT the same way.
        let RouteCredential::Session { token, csrf } = route_credential(&request)? else {
            return Err(authentication_unavailable());
        };
        let session = self
            .verifier
            .verify(token)
            .await
            .map_err(|_| unauthorized())?;
        let claims = session.claims();
        if let Some(required) = csrf {
            check_csrf(required, claims.csrf.as_deref(), request.headers)?;
        }
        Ok(AuthenticatedCaller::new(
            request.attachment_id,
            claims.sub.as_str(),
            CredentialKind::Session,
            self.release
                .grants()
                .permissions(&claims.roles)
                .into_iter()
                .collect(),
        ))
    }
}
