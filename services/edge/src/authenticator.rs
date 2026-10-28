//! The edge's credential mechanism for a protected route.
//!
//! A session token is verified over the key file that the release installs,
//! and its roles map to permissions through the release grants. The box keeps
//! no session state and runs no active-session read, so a verified token's
//! roles hold until the token expires, 930 seconds at most (spec section 4.4).
//! A PAT has no mechanism on the box.

use std::sync::Arc;

use wamn_engine::flow_http_routing::{
    AuthRejection, AuthenticatedCaller, AuthenticationRequest, CredentialType, RouteAuthenticator,
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
    ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
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
        Ok(Some(AuthenticatedCaller::new(
            request.attachment_id,
            claims.sub.as_str(),
            CredentialType::Session,
            false,
            self.release
                .grants()
                .permissions(&claims.roles)
                .iter()
                .map(|permission| wamn_catalog::sealed_operation_reference(permission).to_owned())
                .collect(),
            match claims.roles.as_slice() {
                [role] => Some(role.as_str().into()),
                _ => None,
            },
        )))
    }
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ring::signature::{Ed25519KeyPair, KeyPair as _};
    use serde_json::json;
    use wamn_catalog::{
        AttachmentAuthPolicy, AttachmentRef, AttachmentType, DefinitionHash, RouteAttachment,
        ServedAttachment,
    };
    use wamn_engine::flow_http_routing::Header;
    use wamn_session::token::{SessionAuthority, SessionClaims, SessionHeader};

    use super::*;

    const ISSUER: &str = "https://identity.example.test";
    const AUDIENCE: &str = "tenant-a/edge";

    struct Fixture {
        authenticator: EdgeAuthenticator,
        pair: Ed25519KeyPair,
        attachment: RouteAttachment,
    }

    fn claims() -> SessionClaims {
        let now = chrono::Utc::now().timestamp();
        SessionClaims {
            iss: ISSUER.into(),
            sub: "0b5c7f3e-9a41-4d2e-8f6a-1c2d3e4f5a6b".into(),
            org: "org-a".into(),
            aud: AUDIENCE.into(),
            roles: vec!["operator".into()],
            exp: now + 300,
            iat: now - 10,
            jti: "token-one".into(),
            authority: SessionAuthority::Login("0b5c7f3e-9a41-4d2e-8f6a-1c2d3e4f5a6b".into()),
            csrf: Some("b6a2aca6b182e1a327dbc1bedc8e32cd04c0c9bfb9f3953687b047a4de985ff6".into()),
        }
    }

    impl Fixture {
        async fn new() -> Self {
            let pair = Ed25519KeyPair::from_seed_unchecked(&[1; 32]).unwrap();
            let path =
                std::env::temp_dir().join(format!("edge-auth-{}.json", uuid::Uuid::new_v4()));
            std::fs::write(
                &path,
                serde_json::to_vec(&json!({"keys":[{
                    "kid":"known", "kty":"OKP", "crv":"Ed25519", "alg":"Ed25519", "use":"sig",
                    "x":URL_SAFE_NO_PAD.encode(pair.public_key().as_ref())
                }]}))
                .unwrap(),
            )
            .unwrap();
            let keys = FileKeys::load(&path, ISSUER).unwrap();
            std::fs::remove_file(path).unwrap();
            let verifier = SessionVerifier::new(keys, "org-a", AUDIENCE).unwrap();
            Self {
                authenticator: EdgeAuthenticator::new(
                    verifier,
                    Arc::new(crate::release::test_util::release().await),
                ),
                pair,
                attachment: RouteAttachment {
                    type_: AttachmentType::Http,
                    package_id: "scale".into(),
                    component: "device".into(),
                    operation: crate::release::test_util::OPERATION.into(),
                    definition_hash: DefinitionHash::parse(wamn_catalog::edge_bundle::file_digest(
                        b"route",
                    ))
                    .unwrap(),
                    definition: json!({}),
                    auth_policy: json!({"modes":["session"]}),
                    registered_operation: None,
                },
            }
        }

        fn token(&self, claims: &SessionClaims) -> String {
            let header =
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&SessionHeader::new("known")).unwrap());
            let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
            let signed = format!("{header}.{body}");
            format!(
                "{signed}.{}",
                URL_SAFE_NO_PAD.encode(self.pair.sign(signed.as_bytes()).as_ref())
            )
        }

        async fn authenticate(
            &self,
            headers: &[Header],
        ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
            let release = self.authenticator.release.release();
            self.authenticator
                .authenticate(AuthenticationRequest {
                    manifest: release.manifest(),
                    scope: release.scope(),
                    attachment_id: "record-http",
                    attachment: ServedAttachment::Authored(AttachmentRef::Route(&self.attachment)),
                    policy: AttachmentAuthPolicy::Session,
                    headers,
                    body: b"{}",
                })
                .await
        }

        async fn bearer(&self, token: &str) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
            self.authenticate(&[Header {
                name: "authorization".into(),
                value: format!("Bearer {token}"),
            }])
            .await
        }
    }

    #[tokio::test]
    async fn expired_session_refuses_401() {
        let fixture = Fixture::new().await;
        let mut claims = claims();
        assert!(
            fixture
                .bearer(&fixture.token(&claims))
                .await
                .unwrap()
                .is_some()
        );
        claims.iat -= 600;
        claims.exp -= 600;
        assert_eq!(
            fixture
                .bearer(&fixture.token(&claims))
                .await
                .unwrap_err()
                .status,
            401
        );
    }

    #[tokio::test]
    async fn a_known_key_with_wrong_audience_or_issuer_refuses_401() {
        let fixture = Fixture::new().await;
        let claims = claims();
        assert!(
            fixture
                .bearer(&fixture.token(&claims))
                .await
                .unwrap()
                .is_some()
        );
        for claims in [
            SessionClaims {
                aud: "tenant-b/edge".into(),
                ..claims.clone()
            },
            SessionClaims {
                iss: "https://other.example.test".into(),
                ..claims
            },
        ] {
            assert_eq!(
                fixture
                    .bearer(&fixture.token(&claims))
                    .await
                    .unwrap_err()
                    .status,
                401
            );
        }
    }

    #[tokio::test]
    async fn a_session_cookie_with_mismatched_csrf_refuses_401() {
        let fixture = Fixture::new().await;
        let token = fixture.token(&claims());
        let mut headers = [
            Header {
                name: "cookie".into(),
                value: format!("__Host-wamn-session={token}"),
            },
            Header {
                name: "x-wamn-csrf".into(),
                value: "csrf-one".into(),
            },
        ];
        assert!(fixture.authenticate(&headers).await.unwrap().is_some());
        headers[1].value = "csrf-other".into();
        assert_eq!(
            fixture.authenticate(&headers).await.unwrap_err().status,
            401
        );
    }

    #[tokio::test]
    async fn a_pat_bearer_refuses_401() {
        let fixture = Fixture::new().await;
        assert_eq!(
            fixture.bearer("wamn_pat_fixture").await.unwrap_err().status,
            401
        );
    }
}
