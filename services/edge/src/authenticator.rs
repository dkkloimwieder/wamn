//! Protected edge routes refuse requests pending the edge identity design.

use wamn_engine::ingress::{
    AuthRejection, AuthenticatedCaller, AuthenticationRequest, RouteAuthenticator,
    signature_unauthorized, unauthorized,
};

/// The device does not admit callers to protected HTTP routes.
#[derive(Debug)]
pub struct EdgeAuthenticator;

#[async_trait::async_trait]
impl RouteAuthenticator for EdgeAuthenticator {
    async fn authenticate(
        &self,
        request: AuthenticationRequest<'_>,
    ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
        Err(match request.policy {
            wamn_package::AttachmentAuthPolicy::Signature { .. } => signature_unauthorized(),
            _ => unauthorized(),
        })
    }
}
