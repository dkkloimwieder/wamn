//! Platform authentication for protected routes.
//!
//! The route plugin in [`wamn_engine::flow_http_routing`] resolves each
//! attachment and its policy. For a policy that names a credential it calls
//! [`PlatformRouteAuthenticator`]. That authenticator verifies a PAT through
//! the system identity reader, or a session through the issuer keys. It then
//! reads the caller's permissions through the existing callable-HTTP
//! project pool. Each read answer stays in the host's
//! [auth memory](wamn_authority::auth_cache), so a repeated caller costs no read.
//! A signature policy verifies the raw body with the bound connection's
//! credential and returns its active service principal.

use std::sync::Arc;

use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::Sha256;
use tracing::Instrument as _;
use wamn_catalog::AttachmentAuthPolicy;
use wamn_engine::flow_http_routing::{
    AuthRejection, AuthenticatedCaller, AuthenticationRequest, CredentialType, Header,
    RouteAuthenticator, RouteCredential, authentication_unavailable, require_csrf,
    required_bearer_token, route_credential, signature_unauthorized, unauthorized,
};
use wamn_identity::PrincipalType;
use wamn_identity_protocol::verifier::SessionVerifier;

use crate::authorization::{AuthorizationStore, SessionGrantRefusal};
use crate::session_keys::IssuerKeys;
use wamn_connection::connection_facts::ConnectionFacts;
use wamn_connection::connection_memory::ConnectionStatus;

async fn admit_session<T>(
    verifier: &SessionVerifier<IssuerKeys>,
    token: &str,
    cookie_headers: Option<(&[Header], bool)>,
    permissions: impl AsyncFnOnce(
        &wamn_identity_protocol::token::SessionClaims,
    ) -> Result<T, AuthRejection>,
) -> Result<(wamn_identity_protocol::PrincipalId, T), AuthRejection> {
    let session = verifier.verify(token).await.map_err(|_| unauthorized())?;
    if let Some((headers, requires_csrf)) = cookie_headers {
        require_csrf(requires_csrf, session.claims().csrf.as_ref(), headers)?;
    }
    let permissions = permissions(session.claims()).await?;
    // Permission I/O cannot extend the evidence that admits a request.
    if session.require_admission().is_err() {
        verifier.verify(token).await.map_err(|_| unauthorized())?;
    }
    Ok((session.claims().sub, permissions))
}

/// The application host's read of the status of its environment
/// (docs/architecture/execution.md#routing-and-availability): its row of
/// `platform.environment`, as `wamn_app`, kept in the host's [auth
/// memory](wamn_authority::auth_cache).
pub struct EnvironmentStatusReader {
    postgres: Arc<dyn AuthorizationStore>,
}

impl std::fmt::Debug for EnvironmentStatusReader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EnvironmentStatusReader")
            .finish_non_exhaustive()
    }
}

impl EnvironmentStatusReader {
    pub fn new<S: AuthorizationStore + 'static>(postgres: Arc<S>) -> Self {
        Self { postgres }
    }
}

#[async_trait::async_trait]
impl wamn_engine::flow_http_routing::EnvironmentStatus for EnvironmentStatusReader {
    async fn inactive(
        &self,
        project_env_key: &wamn_registry::ProjectEnvKey,
    ) -> anyhow::Result<bool> {
        let target = wamn_registry::ProjectEnvKey::triple(project_env_key);
        self.postgres
            .environment_inactive(&target.project, project_env_key)
            .await
    }
}

/// The host's authentication authority for PAT, session and signed requests.
#[derive(Debug)]
pub struct PlatformRouteAuthenticator {
    postgres: Arc<dyn AuthorizationStore>,
    projects: std::collections::HashMap<wamn_registry::ProjectEnvKey, Arc<ConnectionFacts>>,
    connection_cache: Option<Arc<dyn wamn_connection::connection_facts::ConnectionCache>>,
    identity_reader: Option<sqlx::PgPool>,
    verifiers: std::collections::HashMap<wamn_registry::ProjectEnvKey, SessionVerifier<IssuerKeys>>,
}

impl PlatformRouteAuthenticator {
    /// Bind the host's project and optional identity reader and session verifier.
    pub fn new<S: AuthorizationStore + 'static>(
        postgres: Arc<S>,
        facts: impl IntoIterator<Item = Arc<ConnectionFacts>>,
        identity: Option<(
            sqlx::PgPool,
            std::collections::HashMap<wamn_registry::ProjectEnvKey, SessionVerifier<IssuerKeys>>,
        )>,
    ) -> Self {
        let projects: std::collections::HashMap<_, _> = facts
            .into_iter()
            .map(|facts| (facts.project_env_key().to_owned(), facts))
            .collect();
        let (identity_reader, verifiers) = match identity {
            Some((reader, verifiers)) => (Some(reader), verifiers),
            None => (None, std::collections::HashMap::new()),
        };
        Self {
            postgres,
            projects,
            connection_cache: None,
            identity_reader,
            verifiers,
        }
    }

    pub fn with_connection_cache(
        mut self,
        cache: Arc<dyn wamn_connection::connection_facts::ConnectionCache>,
    ) -> Self {
        self.connection_cache = Some(cache);
        self
    }

    fn connection_facts(
        &self,
        project: &wamn_registry::ProjectEnvKey,
    ) -> Result<Arc<ConnectionFacts>, AuthRejection> {
        if let Some(facts) = self.projects.get(project) {
            return Ok(Arc::clone(facts));
        }
        Arc::clone(
            self.connection_cache
                .as_ref()
                .ok_or_else(authentication_unavailable)?,
        )
        .project_facts(project)
        .map_err(|_| authentication_unavailable())
    }

    /// Construct route authority without connection credentials for interface tests.
    #[cfg(any(test, feature = "test-util"))]
    pub fn for_test<S: AuthorizationStore + 'static>(
        postgres: Arc<S>,
        project_env_key: &wamn_registry::ProjectEnvKey,
        identity_reader: Option<sqlx::PgPool>,
        verifier: Option<SessionVerifier<IssuerKeys>>,
    ) -> Self {
        Self::new(
            postgres,
            [],
            identity_reader.map(|reader| {
                (
                    reader,
                    verifier
                        .into_iter()
                        .map(|verifier| (project_env_key.to_owned(), verifier))
                        .collect(),
                )
            }),
        )
    }

    async fn authenticate_signature(
        &self,
        facts: &ConnectionFacts,
        header: &str,
        credential: &str,
        headers: &[Header],
        body: &[u8],
    ) -> Result<wamn_identity_protocol::PrincipalId, AuthRejection> {
        let mut values = headers
            .iter()
            .filter(|value| value.name.eq_ignore_ascii_case(header));
        let value = values.next().ok_or_else(signature_unauthorized)?;
        if values.next().is_some() {
            return Err(signature_unauthorized());
        }
        let signature = value
            .value
            .strip_prefix("sha256=")
            .ok_or_else(signature_unauthorized)?;
        let signature = format!("sha256:{signature}")
            .parse::<wamn_digest::Digest>()
            .map_err(|_| signature_unauthorized())?;
        let name = facts
            .reader()
            .connection_name(credential)
            .map_err(|_| signature_unauthorized())?;
        let connections = facts
            .env_connections()
            .await
            .map_err(|_| signature_unauthorized())?;
        let connection = connections
            .connections
            .get(name)
            .ok_or_else(signature_unauthorized)?;
        let fact = facts
            .connection(connection)
            .await
            .map_err(|_| signature_unauthorized())?
            .filter(|fact| fact.status == ConnectionStatus::Active)
            .ok_or_else(signature_unauthorized)?;
        let credential = fact.credential_id.ok_or_else(signature_unauthorized)?;
        let secret = facts
            .secret(&credential)
            .await
            .map_err(|_| signature_unauthorized())?;
        let secret: SignatureSecret =
            serde_json::from_str(secret.expose()).map_err(|_| signature_unauthorized())?;
        if secret.hmac.algorithm != "sha256" {
            return Err(signature_unauthorized());
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.hmac.secret.as_bytes())
            .map_err(|_| signature_unauthorized())?;
        mac.update(body);
        mac.verify_slice(signature.as_bytes())
            .map_err(|_| signature_unauthorized())?;
        Ok(fact.service_principal_id)
    }

    /// `cookie_headers` is `Some` only when `token` arrived by the session
    /// cookie; those requests then pass the CSRF test. Its flag is whether
    /// the route requires the CSRF header, which a read route does not.
    async fn authenticate_session(
        &self,
        attachment_id: &wamn_catalog::canonical_name::CanonicalName,
        token: &str,
        cookie_headers: Option<(&[Header], bool)>,
        project: &wamn_registry::ProjectId,
        project_env_key: &wamn_registry::ProjectEnvKey,
    ) -> Result<AuthenticatedCaller, AuthRejection> {
        let verifier = self
            .verifiers
            .get(project_env_key)
            .ok_or_else(unauthorized)?;
        let (principal, permissions) =
            admit_session(verifier, token, cookie_headers, async |claims| {
                self.postgres
                    .session_operation_permissions(
                        project,
                        project_env_key,
                        &claims.roles,
                        &claims.sub,
                    )
                    .instrument(tracing::info_span!("wamn.auth.permissions"))
                    .await
                    .map_err(|error| {
                        if error.downcast_ref::<SessionGrantRefusal>().is_some() {
                            unauthorized()
                        } else {
                            tracing::warn!(error = %error, "route operation grants unavailable");
                            authentication_unavailable()
                        }
                    })
            })
            .await?;
        let active_role = permissions.active_role();
        Ok(AuthenticatedCaller::new(
            attachment_id.clone(),
            principal,
            CredentialType::Session,
            permissions.admin,
            permissions.references.into_iter().collect(),
            active_role,
        ))
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SignatureSecret {
    hmac: SignatureHmac,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SignatureHmac {
    algorithm: String,
    secret: String,
}

#[async_trait::async_trait]
impl RouteAuthenticator for PlatformRouteAuthenticator {
    async fn authenticate(
        &self,
        request: AuthenticationRequest<'_>,
    ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
        let coordinate = wamn_registry::ProjectEnvKey::triple(request.target);
        let project = &coordinate.project;
        if let AttachmentAuthPolicy::Signature { header, credential } = request.policy {
            let principal = self
                .authenticate_signature(
                    self.connection_facts(request.target)?.as_ref(),
                    header,
                    credential,
                    request.headers,
                    request.body,
                )
                .await?;
            let grants = self
                .postgres
                .principal_grants(project, request.target, &principal)
                .await
                .map_err(|_| authentication_unavailable())?;
            if !grants.active || grants.user {
                return Err(signature_unauthorized());
            }
            let permissions = self
                .postgres
                .user_operation_permissions(project, request.target, &principal)
                .await
                .map_err(|_| authentication_unavailable())?;
            return Ok(Some(AuthenticatedCaller::new(
                request.attachment_id.clone(),
                principal,
                CredentialType::Connection,
                permissions.admin,
                permissions.references.into_iter().collect(),
                None,
            )));
        }
        let AuthenticationRequest {
            target,
            attachment_id,
            headers,
            ..
        } = request;
        if let RouteCredential::Session { token, csrf } = route_credential(&request)? {
            return self
                .authenticate_session(
                    attachment_id,
                    token,
                    csrf.map(|required| (headers, required)),
                    project,
                    target,
                )
                .await
                .map(Some);
        }
        let span = tracing::info_span!(
            target: "wamn::route",
            "wamn.route.authenticate",
            wamn.attachment_id = %attachment_id,
        );
        async {
            let identity_reader = self
                .identity_reader
                .as_ref()
                .ok_or_else(authentication_unavailable)?;
            let token = required_bearer_token(headers)?;
            let principal = self
                .postgres
                .route_pat(project, target, identity_reader, token)
                .instrument(tracing::info_span!("wamn.auth.identity"))
                .await
                .map_err(|error| {
                    tracing::warn!(error = %error, "route PAT authentication unavailable");
                    authentication_unavailable()
                })?
                .ok_or_else(unauthorized)?;
            let credential_deadline = principal.pat_deadline();
            let principal = principal.principal();
            match principal.principal_type() {
                PrincipalType::Platform | PrincipalType::User => return Err(unauthorized()),
                PrincipalType::Service if principal.subject().starts_with("connection-") => {
                    let connection = principal
                        .subject()
                        .strip_prefix("connection-")
                        .and_then(|id| id.parse::<uuid::Uuid>().ok())
                        .ok_or_else(unauthorized)?;
                    let facts = self.connection_facts(target)?;
                    let active = facts
                        .connection(&connection)
                        .await
                        .map_err(|_| authentication_unavailable())?;
                    if !active.is_some_and(|fact| {
                        fact.status == ConnectionStatus::Active
                            && fact.service_principal_id == *principal.id()
                    }) {
                        return Err(unauthorized());
                    }
                }
                PrincipalType::Service => {}
            }
            // A service receives its roles through `wamn grant-role`.
            let permissions = self
                .postgres
                .user_operation_permissions(project, target, principal.id())
                .instrument(tracing::info_span!("wamn.auth.permissions"))
                .await
                .map_err(|error| {
                    tracing::warn!(error = %error, "route operation grants unavailable");
                    authentication_unavailable()
                })?;
            if credential_deadline.is_none_or(|deadline| std::time::SystemTime::now() >= deadline) {
                return Err(unauthorized());
            }
            let active_role = permissions.pat_role();
            Ok(Some(AuthenticatedCaller::new(
                attachment_id.clone(),
                *principal.id(),
                CredentialType::Pat,
                permissions.admin,
                permissions.references.into_iter().collect(),
                active_role,
            )))
        }
        .instrument(span)
        .await
    }
}

/// Admits a request to a route of the control serving root
/// (docs/architecture/execution.md#control-sessions).
///
pub struct ControlRouteAuthenticator {
    verifier: SessionVerifier<IssuerKeys>,
}

impl std::fmt::Debug for ControlRouteAuthenticator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlRouteAuthenticator")
            .finish_non_exhaustive()
    }
}

impl ControlRouteAuthenticator {
    /// `verifier` is bound to the org and its control audience.
    pub fn new(verifier: SessionVerifier<IssuerKeys>) -> Self {
        Self { verifier }
    }
}

impl ControlRouteAuthenticator {
    /// Authenticate an org control endpoint without an application release.
    pub async fn authenticate_control(
        &self,
        attachment_id: &wamn_catalog::canonical_name::CanonicalName,
        read: bool,
        headers: &[wamn_engine::flow_http_routing::Header],
    ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
        use wamn_engine::flow_http_routing::session_cookie;
        let cookie = session_cookie(headers)?;
        if cookie.is_some()
            && headers
                .iter()
                .any(|header| header.name.eq_ignore_ascii_case("authorization"))
        {
            return Err(unauthorized());
        }
        let (token, csrf) = match cookie {
            Some(token) => (token, Some(!read)),
            None => (required_bearer_token(headers)?, None),
        };
        let (principal, ()) = admit_session(
            &self.verifier,
            token,
            csrf.map(|required| (headers, required)),
            async |_| Ok(()),
        )
        .await?;
        Ok(Some(AuthenticatedCaller::new(
            attachment_id.clone(),
            principal,
            CredentialType::Session,
            false,
            std::collections::HashSet::new(),
            None,
        )))
    }
}

#[async_trait::async_trait]
impl RouteAuthenticator for ControlRouteAuthenticator {
    async fn authenticate(
        &self,
        request: AuthenticationRequest<'_>,
    ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
        self.authenticate_control(request.attachment_id, request.read, request.headers)
            .await
    }
}
