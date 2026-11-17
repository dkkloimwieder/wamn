//! Protected edge routes refuse requests until the edge identity design is defined.

use wamn_engine::flow_http_routing::{
    AuthRejection, AuthenticatedCaller, AuthenticationRequest, RouteAuthenticator, RouteCredential,
    route_credential, unauthorized,
};
use wamn_identity_protocol::file_keys::FileKeys;
use wamn_identity_protocol::verifier::SessionVerifier;

/// Legacy edge verifier with protected routes closed.
pub struct EdgeAuthenticator {
    verifier: SessionVerifier<FileKeys>,
}

impl std::fmt::Debug for EdgeAuthenticator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EdgeAuthenticator")
            .finish_non_exhaustive()
    }
}

impl EdgeAuthenticator {
    /// Retain the configured legacy key source while protected routes are closed.
    pub fn new(verifier: SessionVerifier<FileKeys>) -> Self {
        Self { verifier }
    }
}

#[async_trait::async_trait]
impl RouteAuthenticator for EdgeAuthenticator {
    async fn authenticate(
        &self,
        request: AuthenticationRequest<'_>,
    ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
        let RouteCredential::Session { token, .. } = route_credential(&request)? else {
            return Err(unauthorized());
        };
        self.verifier
            .verify(token)
            .await
            .map_err(|_| unauthorized())?;
        Err(unauthorized())
    }
}
