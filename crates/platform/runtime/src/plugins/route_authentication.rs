//! Platform authentication for protected routes.
//!
//! The route plugin in [`wamn_engine::flow_http_routing`] resolves each
//! attachment and its policy. For a policy that names a credential it calls
//! [`PlatformRouteAuthenticator`]. That authenticator verifies a PAT through
//! the system identity reader, or a session through the issuer keys. It then
//! loads the caller's exact permission set through the existing callable-HTTP
//! project pool.

use std::sync::Arc;

use tracing::Instrument as _;
use wamn_engine::flow_http_routing::{
    AuthRejection, AuthenticatedCaller, AuthenticationRequest, CredentialKind, Header,
    RouteAuthenticator, authentication_unavailable, bearer_token, check_csrf,
    required_bearer_token, serves_read, session_cookie, unauthorized,
};
use wamn_platform_identity::{PAT_TOKEN_PREFIX, PreparedIdentityReads, PrincipalKind};
use wamn_session::verifier::SessionVerifier;

use crate::session_keys::IssuerKeys;

const ROUTE_CALLER_ROLE: &str = "route-caller";

/// Resolve an admitted service against its current tenant status and role grants.
pub async fn queued_service_caller(
    client: &(impl tokio_postgres::GenericClient + Sync),
    tenant: &str,
    principal_id: &str,
) -> anyhow::Result<AuthenticatedCaller> {
    let rows = client
        .query(
            "SELECT users.id::text, permissions.permission \
             FROM app_system.users AS users \
             LEFT JOIN app_system.user_roles AS user_roles \
               ON user_roles.tenant_id = users.tenant_id AND user_roles.user_id = users.id \
             LEFT JOIN app_system.permissions AS permissions \
               ON permissions.tenant_id = user_roles.tenant_id \
              AND permissions.role_name = user_roles.role_name \
             WHERE users.tenant_id = $1 AND users.id = $2::text::uuid \
               AND users.type = 'service' AND users.status = 'active'",
            &[&tenant, &principal_id],
        )
        .await?;
    let principal = rows
        .first()
        .ok_or_else(|| anyhow::anyhow!("queued service principal is absent or inactive"))?
        .try_get::<_, String>(0)?;
    let permissions = rows
        .iter()
        .map(|row| row.try_get::<_, Option<String>>(1))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    Ok(AuthenticatedCaller::new(
        "automation",
        principal,
        CredentialKind::QueuedService,
        permissions,
    ))
}

/// Trusted dependencies and scope for PAT-backed route authentication.
pub struct RouteAuthentication {
    identity_reader: Arc<tokio_postgres::Client>,
    /// The identity statement, parsed once at construction rather than on
    /// every request. See [`PreparedIdentityReads`].
    prepared: PreparedIdentityReads,
    postgres: Arc<crate::plugins::wamn_postgres::WamnPostgres>,
    org: Box<str>,
    project: Box<str>,
    expected_subject: Box<str>,
}

impl std::fmt::Debug for RouteAuthentication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RouteAuthentication")
            .field("org", &self.org)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

impl RouteAuthentication {
    /// Bind the two read authorities to trusted package coordinates.
    ///
    /// Environment and tenant remain single-sourced from the loaded release.
    /// Async because it parses the identity statements on `identity_reader`
    /// here, once, instead of on every request. A reader that cannot parse them
    /// cannot authenticate anything, so this fails at startup rather than on the
    /// first request.
    pub async fn new(
        identity_reader: Arc<tokio_postgres::Client>,
        postgres: Arc<crate::plugins::wamn_postgres::WamnPostgres>,
        org: impl Into<Box<str>>,
        project: impl Into<Box<str>>,
        expected_subject: impl Into<Box<str>>,
    ) -> Result<Self, wamn_platform_identity::IdentityError> {
        let prepared = PreparedIdentityReads::prepare(identity_reader.as_ref()).await?;
        Ok(Self {
            identity_reader,
            prepared,
            postgres,
            org: org.into(),
            project: project.into(),
            expected_subject: expected_subject.into(),
        })
    }
}

/// Session authentication with current identity and tenant permission reads.
pub struct SessionRouteAuthentication {
    identity_reader: Arc<tokio_postgres::Client>,
    verifier: SessionVerifier<IssuerKeys>,
    postgres: Arc<crate::plugins::wamn_postgres::WamnPostgres>,
    project: Box<str>,
}

impl std::fmt::Debug for SessionRouteAuthentication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionRouteAuthentication")
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

impl SessionRouteAuthentication {
    /// Bind the configured verifier to the host's existing permission authority.
    pub fn new(
        verifier: SessionVerifier<IssuerKeys>,
        identity_reader: Arc<tokio_postgres::Client>,
        postgres: Arc<crate::plugins::wamn_postgres::WamnPostgres>,
        project: impl Into<Box<str>>,
    ) -> Self {
        Self {
            identity_reader,
            verifier,
            postgres,
            project: project.into(),
        }
    }
}

/// The platform's route authenticator: PAT and session, each present only when
/// the host configured it.
///
/// A cloud host always installs one. An empty authenticator refuses a session
/// route as unauthorized and a PAT route as authentication-unavailable.
#[derive(Debug, Default)]
pub struct PlatformRouteAuthenticator {
    authentication: Option<Arc<RouteAuthentication>>,
    session_authentication: Option<Arc<SessionRouteAuthentication>>,
}

impl PlatformRouteAuthenticator {
    /// Enable PAT authentication with host-selected database authorities.
    #[must_use]
    pub fn with_authentication(mut self, authentication: Arc<RouteAuthentication>) -> Self {
        self.authentication = Some(authentication);
        self
    }

    /// Supply public verification keys and the scoped tenant permission reader.
    #[must_use]
    pub fn with_session_authentication(
        mut self,
        authentication: Arc<SessionRouteAuthentication>,
    ) -> Self {
        self.session_authentication = Some(authentication);
        self
    }

    /// `cookie_headers` is `Some` only when `token` arrived by the session
    /// cookie; those requests then pass the CSRF check. Its flag is whether
    /// the route requires the CSRF header, which a read route does not.
    async fn authenticate_session(
        &self,
        attachment_id: &str,
        token: &str,
        cookie_headers: Option<(&[Header], bool)>,
        tenant: &str,
        environment: &str,
    ) -> Result<AuthenticatedCaller, AuthRejection> {
        let authentication = self
            .session_authentication
            .as_ref()
            .ok_or_else(unauthorized)?;
        let session = authentication
            .verifier
            .verify(token)
            .await
            .map_err(|_| unauthorized())?;
        if let Some((headers, requires_csrf)) = cookie_headers {
            check_csrf(requires_csrf, session.claims().csrf.as_deref(), headers)?;
        }
        let principal = session.claims().sub.parse().map_err(|_| unauthorized())?;
        let permissions = authentication
            .postgres
            .session_operation_permissions(
                &authentication.project,
                tenant,
                &session.claims().roles,
                &principal,
            )
            .instrument(tracing::info_span!("wamn.auth.permissions"))
            .await
            .map_err(|error| {
                tracing::warn!(error = %error, "route operation grants unavailable");
                authentication_unavailable()
            })?;
        let active = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            wamn_platform_identity::session_token::session_is_active(
                authentication.identity_reader.as_ref(),
                session.claims(),
                &authentication.project,
                environment,
            ),
        )
        .await
        .map_err(|_| authentication_unavailable())?
        .map_err(|_| authentication_unavailable())?;
        if !active {
            return Err(unauthorized());
        }
        // Permission I/O cannot extend the evidence that admitted this request.
        // Once returned, nested work retains this caller without reauthentication.
        // If the key evidence expired during that I/O, the same token is
        // verified once more on fresh keys, and a token past its own age still
        // refuses (wamn-co0p).
        if session.check_admission().is_err() {
            authentication
                .verifier
                .verify(token)
                .await
                .map_err(|_| unauthorized())?;
        }
        Ok(AuthenticatedCaller::new(
            attachment_id,
            session.claims().sub.as_str(),
            CredentialKind::Session,
            permissions.into_iter().collect(),
        ))
    }
}

#[async_trait::async_trait]
impl RouteAuthenticator for PlatformRouteAuthenticator {
    async fn authenticate(
        &self,
        request: AuthenticationRequest<'_>,
    ) -> Result<AuthenticatedCaller, AuthRejection> {
        let AuthenticationRequest {
            manifest,
            attachment_id,
            attachment,
            policy,
            headers,
        } = request;
        let cookie = session_cookie(headers)?;
        let has_authorization = headers
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("authorization"));
        if has_authorization && cookie.is_some() {
            return Err(unauthorized());
        }
        if let Some(token) = cookie.filter(|_| policy.allows_session()) {
            return self
                .authenticate_session(
                    attachment_id,
                    token,
                    Some((headers, !serves_read(manifest, attachment))),
                    &manifest.release.tenant_id,
                    &manifest.release.environment,
                )
                .await;
        }
        // The wire shape selects one mechanism; failed authentication never
        // falls back to another credential or repeats an executed operation.
        let session = policy.allows_session()
            && (!policy.allows_pat()
                || bearer_token(headers).is_some_and(|token| !token.starts_with(PAT_TOKEN_PREFIX)));
        if session {
            let token = required_bearer_token(headers)?;
            return self
                .authenticate_session(
                    attachment_id,
                    token,
                    None,
                    &manifest.release.tenant_id,
                    &manifest.release.environment,
                )
                .await;
        }
        let span = tracing::info_span!(
            target: "wamn::route",
            "wamn.route.authenticate",
            wamn.attachment_id = %attachment_id,
        );
        async {
            let authentication = self
                .authentication
                .as_ref()
                .ok_or_else(authentication_unavailable)?;
            let token = required_bearer_token(headers)?;
            let principal = authentication
                .prepared
                .authenticate_route_pat(
                    authentication.identity_reader.as_ref(),
                    token,
                    &authentication.org,
                    &authentication.project,
                    &manifest.release.environment,
                    ROUTE_CALLER_ROLE,
                )
                .instrument(tracing::info_span!("wamn.auth.identity"))
                .await
                .map_err(|error| {
                    tracing::warn!(error = %error, "route PAT authentication unavailable");
                    authentication_unavailable()
                })?
                .ok_or_else(unauthorized)?;
            let principal = principal.principal();
            let permissions = match principal.kind() {
                PrincipalKind::Platform => return Err(unauthorized()),
                PrincipalKind::Service => {
                    if principal.subject() != authentication.expected_subject.as_ref() {
                        return Err(unauthorized());
                    }
                    authentication
                        .postgres
                        .operation_permissions(
                            &authentication.project,
                            &manifest.release.tenant_id,
                            ROUTE_CALLER_ROLE,
                        )
                        .instrument(tracing::info_span!("wamn.auth.permissions"))
                        .await
                }
                PrincipalKind::Human => {
                    authentication
                        .postgres
                        .user_operation_permissions(
                            &authentication.project,
                            &manifest.release.tenant_id,
                            principal.id(),
                        )
                        .instrument(tracing::info_span!("wamn.auth.permissions"))
                        .await
                }
            }
            .map_err(|error| {
                tracing::warn!(error = %error, "route operation grants unavailable");
                authentication_unavailable()
            })?;
            Ok(AuthenticatedCaller::new(
                attachment_id,
                principal.id().as_str(),
                CredentialKind::Pat,
                permissions.into_iter().collect(),
            ))
        }
        .instrument(span)
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use serde_json::{Value, json};
    use wamn_catalog::{
        ArtifactHash, AttachmentKind, AttachmentTarget, DefinitionHash, EffectiveReleaseId,
        PAT_AUTHENTICATION_MODE, PackageCoordinate, ServingAttachment, ServingComponent,
        ServingComponentOperation, ServingManifest, ServingRelease, ServingWiring,
    };
    use wamn_engine::flow_http_routing::{FlowHttpRouting, RouteInFlightLimit};
    use wamn_engine::release_manifest::LoadedRelease;

    use super::*;

    const UNAUTHORIZED: (u16, &str) = (401, "unauthorized");
    const UNAVAILABLE: (u16, &str) = (503, "authentication-unavailable");

    /// One HTTP route, `orders`, whose policy has `modes`.
    fn routing(modes: &Value) -> FlowHttpRouting {
        let digest = |hex: char| format!("sha256:{}", hex.to_string().repeat(64));
        let manifest = ServingManifest::new(
            ServingRelease {
                tenant_id: "tenant-a".into(),
                effective_release_id: EffectiveReleaseId::new(7).unwrap(),
                environment: "prod".into(),
                packages: BTreeSet::from([PackageCoordinate::new("cat", "1.0.0").unwrap()]),
            },
            BTreeSet::from([ServingComponent {
                package_id: "cat".into(),
                component: "http-request".into(),
                interface_version: "0.1".into(),
                digest: ArtifactHash::parse(digest('a')).unwrap(),
                operations: BTreeMap::from([(
                    "request".into(),
                    ServingComponentOperation {
                        pre_commit: None,
                        committed_result_schema: None,
                        fresh_only: false,
                        registered_operation: None,
                        permissions: BTreeSet::new(),
                        participant: None,
                        statements: BTreeMap::new(),
                    },
                )]),
            }]),
            BTreeSet::new(),
            BTreeSet::from([ServingWiring {
                package_id: "cat".into(),
                wiring_id: "orders".into(),
                wiring_version: 1,
                graph_hash: DefinitionHash::parse(digest('b')).unwrap(),
            }]),
            BTreeMap::from([(
                "orders".to_string(),
                ServingAttachment {
                    kind: AttachmentKind::Http,
                    package_id: "cat".into(),
                    target: AttachmentTarget::Wiring {
                        wiring_id: "orders".into(),
                        wiring_version: 1,
                    },
                    definition_hash: DefinitionHash::parse(digest('c')).unwrap(),
                    definition: json!({
                        "route": {"host": "api.example.test", "path": "/orders", "method": "POST"}
                    }),
                    auth_policy: json!({"modes": modes}),
                    registered_operation: None,
                },
            )]),
            BTreeMap::new(),
        )
        .expect("fixture manifest is valid");
        let release = LoadedRelease::load_canonical_bytes(&manifest.canonical_bytes(), "fixture")
            .expect("fixture manifest loads");
        FlowHttpRouting::new(Some(Arc::new(release)), RouteInFlightLimit::default())
            .with_authenticator(Arc::new(PlatformRouteAuthenticator::default()))
    }

    #[tokio::test]
    async fn pat_mode_is_recognized_and_an_absent_backend_is_one_generic_outage() {
        let plugin = routing(&json!([PAT_AUTHENTICATION_MODE]));

        let rejection = plugin
            .authenticate_headers_for_test("orders", &[])
            .await
            .expect_err("a protected route without its backend refuses");

        assert_eq!((rejection.0, rejection.1.as_str()), UNAVAILABLE);
    }

    #[tokio::test]
    async fn a_bearer_and_a_session_cookie_together_refuse() {
        let plugin = routing(&json!(["pat", "session"]));

        for authorization in ["Bearer session.token", "Bearer wamn_pat_x", "Basic x"] {
            let rejection = plugin
                .authenticate_headers_for_test(
                    "orders",
                    &[
                        ("authorization", authorization),
                        ("cookie", "__Host-wamn-session=session.token"),
                    ],
                )
                .await
                .expect_err("two credentials refuse");
            assert_eq!((rejection.0, rejection.1.as_str()), UNAUTHORIZED);
        }
    }

    /// Owner ruling on wamn-e5in.5: an empty authenticator answers a session
    /// route as unauthorized and a PAT route as authentication-unavailable.
    #[tokio::test]
    async fn an_empty_authenticator_refuses_a_session_route_401_and_a_pat_route_503() {
        for (modes, expected) in [
            (json!(["session"]), UNAUTHORIZED),
            (json!([PAT_AUTHENTICATION_MODE]), UNAVAILABLE),
        ] {
            let rejection = routing(&modes)
                .authenticate_headers_for_test("orders", &[("authorization", "Bearer token")])
                .await
                .expect_err("an empty authenticator refuses");
            assert_eq!((rejection.0, rejection.1.as_str()), expected);
        }
    }
}
