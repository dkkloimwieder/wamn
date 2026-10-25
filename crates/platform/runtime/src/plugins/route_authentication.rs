//! Platform authentication for protected routes.
//!
//! The route plugin in [`wamn_engine::flow_http_routing`] resolves each
//! attachment and its policy. For a policy that names a credential it calls
//! [`PlatformRouteAuthenticator`]. That authenticator verifies a PAT through
//! the system identity reader, or a session through the issuer keys. It then
//! reads the caller's permissions through the existing callable-HTTP
//! project pool. Each read answer stays in the host's
//! [auth memory](crate::auth_cache), so a repeated caller costs no read.
//! A signature policy verifies the raw body through the host's project
//! credential store and returns no caller principal.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::Sha256;
use tracing::Instrument as _;
use wamn_catalog::AttachmentAuthPolicy;
use wamn_engine::flow_http_routing::{
    AuthRejection, AuthenticatedCaller, AuthenticationRequest, CredentialType, Header,
    RouteAuthenticator, RouteCredential, authentication_unavailable, check_csrf,
    required_bearer_token, route_credential, signature_unauthorized, unauthorized,
};
use wamn_platform_identity::{PreparedIdentityReads, PrincipalType};
use wamn_project_state::ADMIN_ROLE;
use wamn_session::verifier::SessionVerifier;

use crate::plugins::wamn_credentials::WamnCredentials;
use crate::session_keys::IssuerKeys;

/// The user row of one active service or user principal, each role it
/// holds and each permission of that role. A principal without roles yields
/// one row with a null role.
const QUEUED_RUN_CALLER_SQL: &str = "SELECT \"user\".id::text, \"user\".type, user_role.role_name, \
         permission.permission \
    FROM platform.\"user\" AS \"user\" \
    LEFT JOIN platform.user_role AS user_role \
      ON user_role.user_id = \"user\".id \
    LEFT JOIN platform.permission AS permission \
      ON permission.role_name = user_role.role_name \
   WHERE \"user\".id = $1::text::uuid \
     AND \"user\".type IN ('service', 'user') AND \"user\".status = 'active'";

/// Resolve the principal of a queued run against its current environment status
/// and role grants.
///
/// With `role`, the run executes with that one role active
/// (docs/plan/workflows.md R3): the principal is an active service (the
/// `workflow` user or an application principal) or user that holds `role`,
/// its permissions are the rows of `role` only, and it is `admin` only when
/// `role` is `admin`. A principal that does not hold `role` refuses.
///
/// Without `role`, the run is an operator-admitted automation run: the
/// principal is an active service, and its permissions are the rows of every
/// role it holds.
pub async fn queued_run_caller(
    client: &(impl tokio_postgres::GenericClient + Sync),
    principal_id: &str,
    role: Option<&str>,
) -> anyhow::Result<AuthenticatedCaller> {
    let rows = client
        .query(QUEUED_RUN_CALLER_SQL, &[&principal_id])
        .await?;
    let first = rows
        .first()
        .ok_or_else(|| anyhow::anyhow!("queued principal is absent or inactive"))?;
    let principal: String = first.try_get(0)?;
    let principal_type: String = first.try_get(1)?;
    let mut held: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in &rows {
        if let Some(role) = row.try_get::<_, Option<String>>(2)? {
            held.entry(role)
                .or_default()
                .extend(row.try_get::<_, Option<String>>(3)?);
        }
    }
    if let Some(role) = role {
        return role_caller(&principal, role, &held).ok_or_else(|| {
            anyhow::anyhow!("queued principal does not hold the run's role {role:?}")
        });
    }
    anyhow::ensure!(
        principal_type == "service",
        "queued service principal is absent or inactive"
    );
    Ok(AuthenticatedCaller::new(
        "automation",
        principal,
        CredentialType::QueuedService,
        held.contains_key(ADMIN_ROLE),
        held.into_values().flatten().collect(),
        None,
    ))
}

/// The caller of a run that executes as `principal_id` with `role` active,
/// from the roles the principal `held` with their permissions: the
/// permissions of `role` only, and `admin` only when `role` is `admin`. None
/// when the principal does not hold `role`.
pub fn role_caller(
    principal_id: &str,
    role: &str,
    held: &BTreeMap<String, BTreeSet<String>>,
) -> Option<AuthenticatedCaller> {
    let permissions = held.get(role)?;
    Some(AuthenticatedCaller::new(
        "workflow",
        principal_id,
        CredentialType::QueuedService,
        role == ADMIN_ROLE,
        permissions.iter().cloned().collect(),
        Some(role.into()),
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

/// The application host's read of the status of its environment
/// (docs/plan/platform-ui.md §5.4): its row of `platform.environment`, as
/// `wamn_app`, kept in the host's [auth memory](crate::auth_cache).
pub struct EnvironmentStatusReader {
    postgres: Arc<crate::plugins::wamn_postgres::WamnPostgres>,
    project: String,
}

impl std::fmt::Debug for EnvironmentStatusReader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EnvironmentStatusReader")
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

impl EnvironmentStatusReader {
    pub fn new(
        postgres: Arc<crate::plugins::wamn_postgres::WamnPostgres>,
        project: String,
    ) -> Self {
        Self { postgres, project }
    }
}

#[async_trait::async_trait]
impl wamn_engine::flow_http_routing::EnvironmentStatus for EnvironmentStatusReader {
    async fn inactive(&self, tenant: &str) -> anyhow::Result<bool> {
        self.postgres
            .environment_inactive(&self.project, tenant)
            .await
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

/// The platform's route authenticator: PAT, session and signature, present when
/// the host configured it.
///
/// A cloud host always installs one. An empty authenticator refuses a session
/// route as unauthorized and a PAT route as authentication-unavailable.
#[derive(Debug, Default)]
pub struct PlatformRouteAuthenticator {
    authentication: Option<Arc<RouteAuthentication>>,
    session_authentication: Option<Arc<SessionRouteAuthentication>>,
    signature_authentication: Option<(Arc<WamnCredentials>, Box<str>)>,
}

impl PlatformRouteAuthenticator {
    /// Resolve signature credentials under the host's own project.
    #[must_use]
    pub fn with_signature_authentication(
        mut self,
        credentials: Arc<WamnCredentials>,
        project: impl Into<Box<str>>,
    ) -> Self {
        self.signature_authentication = Some((credentials, project.into()));
        self
    }

    fn authenticate_signature(
        &self,
        header: &str,
        credential: &str,
        headers: &[Header],
        body: &[u8],
    ) -> Result<(), AuthRejection> {
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
        if signature.len() != 64
            || !signature
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(signature_unauthorized());
        }
        let signature = hex::decode(signature).map_err(|_| signature_unauthorized())?;
        let (credentials, project) = self
            .signature_authentication
            .as_ref()
            .ok_or_else(signature_unauthorized)?;
        let secret = credentials
            .lookup(project, credential)
            .ok_or_else(signature_unauthorized)?;
        let secret: SignatureSecret =
            serde_json::from_str(&secret).map_err(|_| signature_unauthorized())?;
        if secret.hmac.algorithm != "sha256" {
            return Err(signature_unauthorized());
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.hmac.secret.as_bytes())
            .map_err(|_| signature_unauthorized())?;
        mac.update(body);
        mac.verify_slice(&signature)
            .map_err(|_| signature_unauthorized())
    }

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
        let cache = authentication.postgres.auth_cache();
        if !cache.session_active(token) {
            let ticket = cache.identity_ticket();
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
            cache.keep_session_active(ticket, token, session.claims().sub.as_str());
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
        let active_role = permissions.active_role();
        Ok(AuthenticatedCaller::new(
            attachment_id,
            session.claims().sub.as_str(),
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
        if let AttachmentAuthPolicy::Signature { header, credential } = request.policy {
            self.authenticate_signature(header, credential, request.headers, request.body)?;
            return Ok(None);
        }
        let AuthenticationRequest {
            scope,
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
                    &scope.tenant_id,
                    &scope.environment,
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
            let authentication = self
                .authentication
                .as_ref()
                .ok_or_else(authentication_unavailable)?;
            let token = required_bearer_token(headers)?;
            let cache = authentication.postgres.auth_cache();
            let principal = if let Some(principal) = cache.pat(token) {
                principal
            } else {
                let ticket = cache.identity_ticket();
                let principal = authentication
                    .prepared
                    .authenticate_route_pat(
                        authentication.identity_reader.as_ref(),
                        token,
                        &authentication.org,
                        &authentication.project,
                        &scope.environment,
                        &[ADMIN_ROLE],
                    )
                    .instrument(tracing::info_span!("wamn.auth.identity"))
                    .await
                    .map_err(|error| {
                        tracing::warn!(error = %error, "route PAT authentication unavailable");
                        authentication_unavailable()
                    })?
                    .ok_or_else(unauthorized)?;
                cache.keep_pat(ticket, token, principal.clone());
                principal
            };
            let principal = principal.principal();
            match principal.principal_type() {
                PrincipalType::Platform => return Err(unauthorized()),
                PrincipalType::Service
                    if principal.subject() != authentication.expected_subject.as_ref() =>
                {
                    return Err(unauthorized());
                }
                PrincipalType::Service | PrincipalType::User => {}
            }
            // A service and a user hold their roles the same way, through
            // `wamn-ctl grant-role`.
            let permissions = authentication
                .postgres
                .user_operation_permissions(
                    &authentication.project,
                    &scope.tenant_id,
                    principal.id(),
                )
                .instrument(tracing::info_span!("wamn.auth.permissions"))
                .await
                .map_err(|error| {
                    tracing::warn!(error = %error, "route operation grants unavailable");
                    authentication_unavailable()
                })?;
            let active_role = permissions.active_role();
            Ok(Some(AuthenticatedCaller::new(
                attachment_id,
                principal.id().as_str(),
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
/// (docs/plan/platform-ui.md §4.3).
///
/// Only a browser login session of the host's control audience passes. Every
/// request rechecks, through the `control` login, that the login is live and
/// that its principal still holds a control role in the token's org, so a
/// revoked role takes effect on the next request. The caller holds no
/// application role and no operation grant.
pub struct ControlRouteAuthenticator {
    verifier: SessionVerifier<IssuerKeys>,
    control: Arc<tokio_postgres::Client>,
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
    pub fn new(
        verifier: SessionVerifier<IssuerKeys>,
        control: Arc<tokio_postgres::Client>,
    ) -> Self {
        Self { verifier, control }
    }
}

#[async_trait::async_trait]
impl RouteAuthenticator for ControlRouteAuthenticator {
    async fn authenticate(
        &self,
        request: AuthenticationRequest<'_>,
    ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
        let RouteCredential::Session { token, csrf } = route_credential(&request)? else {
            return Err(unauthorized());
        };
        let session = self
            .verifier
            .verify(token)
            .await
            .map_err(|_| unauthorized())?;
        if let Some(requires_csrf) = csrf {
            check_csrf(
                requires_csrf,
                session.claims().csrf.as_deref(),
                request.headers,
            )?;
        }
        let active = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            wamn_platform_identity::control::control_session_is_active(
                self.control.as_ref(),
                session.claims(),
            ),
        )
        .await
        .map_err(|_| authentication_unavailable())?
        .map_err(|error| {
            tracing::warn!(error = %error, "control authority unavailable");
            authentication_unavailable()
        })?;
        if !active {
            return Err(unauthorized());
        }
        if session.check_admission().is_err() {
            self.verifier
                .verify(token)
                .await
                .map_err(|_| unauthorized())?;
        }
        Ok(Some(AuthenticatedCaller::new(
            request.attachment_id,
            session.claims().sub.as_str(),
            CredentialType::Session,
            false,
            std::collections::HashSet::new(),
            None,
        )))
    }
}

#[cfg(test)]
pub(crate) mod tests {

    use serde_json::{Value, json};
    use wamn_catalog::{
        AttachmentTarget, AttachmentType, DefinitionHash, PAT_AUTHENTICATION_MODE,
        PackageCoordinate, ServingAttachment, ServingComponent, ServingComponentOperation,
        ServingManifest, ServingRelease,
    };
    use wamn_engine::flow_http_routing::{FlowHttpRouting, RouteInFlightLimit};
    use wamn_engine::release_manifest::LoadedRelease;

    use super::*;

    const UNAUTHORIZED: (u16, &str) = (401, "unauthorized");
    const UNAVAILABLE: (u16, &str) = (503, "authentication-unavailable");

    /// One HTTP route, `orders`, whose policy has `modes`.
    pub(crate) fn routing(modes: &Value) -> FlowHttpRouting {
        routing_policy(json!({"modes": modes}))
    }

    fn routing_policy(auth_policy: Value) -> FlowHttpRouting {
        let digest = |hex: char| format!("sha256:{}", hex.to_string().repeat(64));
        let manifest = ServingManifest::new(
            ServingRelease {
                packages: BTreeSet::from([PackageCoordinate::new("cat", "1.0.0").unwrap()]),
            },
            BTreeSet::from([ServingComponent {
                runtime_schema: None,
                statement_instances: std::collections::BTreeMap::new(),
                package_id: "cat".into(),
                operations: BTreeMap::from([(
                    "request".into(),
                    ServingComponentOperation {
                        committed_result_schema: None,
                        registered_operation: None,
                        permissions: BTreeSet::new(),
                        statements: BTreeMap::new(),
                    },
                )]),
                descriptor: wamn_catalog::ComponentDescriptor::named(
                    "http-request",
                    "0.1",
                    digest('a'),
                ),
            }]),
            BTreeSet::from([wamn_catalog::ServingRoute {
                package_id: "cat".into(),
                component: "http-request".into(),
                operation: "request".into(),
                type_: wamn_catalog::OperationType::Command,
                reads: BTreeSet::new(),
                revision: None,
                idempotency: None,
                canonicalization: None,
            }]),
            BTreeMap::from([(
                "orders".to_string(),
                ServingAttachment {
                    type_: AttachmentType::Http,
                    package_id: "cat".into(),
                    target: AttachmentTarget::Route {
                        component: "http-request".into(),
                        operation: "request".into(),
                    },
                    definition_hash: DefinitionHash::parse(digest('c')).unwrap(),
                    definition: json!({
                        "route": {"host": "api.example.test", "path": "/orders", "method": "POST"}
                    }),
                    auth_policy,
                    registered_operation: None,
                },
            )]),
        )
        .expect("fixture manifest is valid");
        let release = LoadedRelease::load_canonical_bytes(
            &manifest.canonical_bytes(),
            "fixture",
            wamn_engine::release_manifest::ReleaseScope::new("tenant-a", "prod").with_applications(
                [(
                    "catalog".to_owned(),
                    wamn_catalog::ApplicationDefinition {
                        packages: ["cat".to_owned()].into(),
                    },
                )]
                .into(),
            ),
        )
        .expect("fixture manifest loads");
        FlowHttpRouting::new(Some(Arc::new(release)), RouteInFlightLimit::default())
            .with_authenticator(Arc::new(PlatformRouteAuthenticator::default()))
    }

    mod signature;

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

    /// A queued run executes with its one role active: a principal holding
    /// roles `a` and `b`, queued with `a`, holds `a`'s operations only; it is
    /// `admin` only when the role is `admin`; a role it does not hold refuses.
    #[test]
    fn a_queued_run_holds_the_operations_of_its_role_only() {
        let held = BTreeMap::from([
            ("a".to_owned(), BTreeSet::from(["app:order/get".to_owned()])),
            ("b".to_owned(), BTreeSet::from(["app:order/put".to_owned()])),
            (ADMIN_ROLE.to_owned(), BTreeSet::new()),
        ]);
        let caller = role_caller("principal", "a", &held).expect("the principal holds a");
        assert!(caller.permits("app:order/get@1.0.0"));
        assert!(!caller.permits("app:order/put@1.0.0"));
        assert!(!caller.admin());
        assert_eq!(caller.active_role(), Some("a"));

        let admin = role_caller("principal", ADMIN_ROLE, &held).expect("the principal holds admin");
        assert!(admin.admin());
        assert!(admin.permits("app:order/put@1.0.0"));

        assert!(role_caller("principal", "c", &held).is_none());
        let without_admin =
            BTreeMap::from([("a".to_owned(), BTreeSet::from(["app:order/get".to_owned()]))]);
        assert!(role_caller("principal", ADMIN_ROLE, &without_admin).is_none());
    }
}
