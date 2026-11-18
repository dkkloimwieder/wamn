//! Platform authentication for protected routes.
//!
//! The route plugin in [`wamn_engine::ingress`] resolves each
//! attachment and its policy. For a policy that names a credential it calls
//! [`PlatformRouteAuthenticator`]. That authenticator verifies a PAT through
//! its project database, or an access token through the configured issuer.
//! It reads authorization from the project database. Signature policies verify the
//! raw body with the bound connection credential and return its active principal.

use std::sync::Arc;

use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::Sha256;
use tracing::Instrument as _;
use wamn_catalog::AttachmentAuthPolicy;
use wamn_engine::ingress::{
    AuthRejection, AuthenticatedCaller, AuthenticationRequest, CredentialType, Header,
    RouteAuthenticator, RouteCredential, authentication_unavailable, required_bearer_token,
    route_credential, signature_unauthorized, unauthorized,
};
use wamn_identity_protocol::secret::PatToken;

use crate::access_token::AccessTokenVerifier;
use crate::authorization::AuthorizationStore;
use wamn_connection::connection_facts::ConnectionFacts;
use wamn_connection::connection_memory::ConnectionStatus;

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
impl wamn_engine::ingress::EnvironmentStatus for EnvironmentStatusReader {
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
    connection_cache: Option<Arc<dyn wamn_connection::connection_facts::ConnectionCache>>,
    verifier: Option<AccessTokenVerifier>,
}

impl PlatformRouteAuthenticator {
    /// Bind a host to its authorization store and configured issuer.
    pub fn new<S: AuthorizationStore + 'static>(
        postgres: Arc<S>,
        verifier: Option<AccessTokenVerifier>,
    ) -> Self {
        Self {
            postgres,
            connection_cache: None,
            verifier,
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
        Arc::clone(
            self.connection_cache
                .as_ref()
                .ok_or_else(authentication_unavailable)?,
        )
        .project_facts(project)
        .map_err(|_| authentication_unavailable())
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

    async fn authenticate_session(
        &self,
        token: &str,
        project: &wamn_registry::ProjectId,
        request: &AuthenticationRequest<'_>,
    ) -> Result<AuthenticatedCaller, AuthRejection> {
        let token = self
            .verifier
            .as_ref()
            .ok_or_else(unauthorized)?
            .verify(token)
            .await
            .map_err(|_| unauthorized())?;
        let principal = self
            .postgres
            .resolve_subject(project, request.target, token.issuer(), token.subject())
            .await
            .map_err(|error| {
                tracing::warn!(error = %error, "project subject read unavailable");
                authentication_unavailable()
            })?
            .ok_or_else(unauthorized)?;
        let permissions = self
            .postgres
            .user_operation_permissions(project, request.target, &principal)
            .instrument(tracing::info_span!("wamn.auth.permissions"))
            .await
            .map_err(|error| {
                tracing::warn!(error = %error, "route operation grants unavailable");
                authentication_unavailable()
            })?;
        if !token.is_current() || !permissions.user {
            return Err(unauthorized());
        }
        let active_role = permissions.active_role();
        Ok(AuthenticatedCaller::new(
            request.attachment_id.clone(),
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
        if request.policy == AttachmentAuthPolicy::None {
            return Ok(None);
        }
        let token = match route_credential(&request)? {
            RouteCredential::Session(token) => {
                return self
                    .authenticate_session(token, project, &request)
                    .await
                    .map(Some);
            }
            RouteCredential::Pat(token) => token,
        };
        let AuthenticationRequest {
            target,
            attachment_id,
            ..
        } = request;
        let span = tracing::info_span!(
            target: "wamn::route",
            "wamn.route.authenticate",
            wamn.attachment_id = %attachment_id,
        );
        async {
            let token = token.parse::<PatToken>().map_err(|_| unauthorized())?;
            let principal = self
                .postgres
                .route_pat(project, target, &token)
                .instrument(tracing::info_span!("wamn.auth.identity"))
                .await
                .map_err(|error| {
                    tracing::warn!(error = %error, "route PAT authentication unavailable");
                    authentication_unavailable()
                })?
                .ok_or_else(unauthorized)?;
            if let Some(id) = principal.subject.strip_prefix("connection-") {
                let connection = id.parse::<uuid::Uuid>().map_err(|_| unauthorized())?;
                let facts = self.connection_facts(target)?;
                let active = facts
                    .connection(&connection)
                    .await
                    .map_err(|_| authentication_unavailable())?;
                if !active.is_some_and(|fact| {
                    fact.status == ConnectionStatus::Active
                        && fact.service_principal_id == principal.principal
                }) {
                    return Err(unauthorized());
                }
            }
            // A service receives its roles through `wamn grant-role`.
            let permissions = self
                .postgres
                .user_operation_permissions(project, target, &principal.principal)
                .instrument(tracing::info_span!("wamn.auth.permissions"))
                .await
                .map_err(|error| {
                    tracing::warn!(error = %error, "route operation grants unavailable");
                    authentication_unavailable()
                })?;
            if std::time::SystemTime::now() >= principal.deadline {
                return Err(unauthorized());
            }
            let active_role = permissions.pat_role();
            Ok(Some(AuthenticatedCaller::new(
                attachment_id.clone(),
                principal.principal,
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

/// Authenticate the bearer identity for a control endpoint. Control reads eligibility in SQL.
#[derive(Debug)]
pub struct ControlRouteAuthenticator {
    verifier: AccessTokenVerifier,
}

impl ControlRouteAuthenticator {
    /// Use the verifier bound to the configured control org.
    pub fn new(verifier: AccessTokenVerifier) -> Self {
        Self { verifier }
    }

    /// Verify the bearer subject and recover the system principal UUID.
    pub async fn authenticate_control(
        &self,
        attachment_id: &wamn_catalog::canonical_name::CanonicalName,
        headers: &[Header],
    ) -> Result<AuthenticatedCaller, AuthRejection> {
        let token = self
            .verifier
            .verify(required_bearer_token(headers)?)
            .await
            .map_err(|_| unauthorized())?;
        let principal = token.subject().parse().map_err(|_| unauthorized())?;
        if !token.is_current() {
            return Err(unauthorized());
        }
        Ok(AuthenticatedCaller::new(
            attachment_id.clone(),
            principal,
            CredentialType::Session,
            false,
            Default::default(),
            None,
        ))
    }
}
