//! Platform authentication for protected routes.
//!
//! The route plugin in [`wamn_engine::flow_http_routing`] resolves each
//! attachment and its policy. For a policy that names a credential it calls
//! [`PlatformRouteAuthenticator`]. That authenticator verifies a PAT through
//! the system identity reader, or a session through the issuer keys. It then
//! reads the caller's permissions through the existing callable-HTTP
//! project pool. Each read answer stays in the host's
//! [auth memory](crate::auth_cache), so a repeated caller costs no read.
//! A signature policy verifies the raw body with the bound connection's
//! credential and returns its active service principal.

#[cfg(test)]
use sqlx::Row as _;
use std::collections::{BTreeMap, BTreeSet};
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
use wamn_identity_protocol::ADMIN_ROLE;
use wamn_identity_protocol::verifier::SessionVerifier;
use wamn_platform_identity::PrincipalType;

use crate::connection_facts::ConnectionFacts;
use crate::connection_memory::ConnectionStatus;
use crate::session_keys::IssuerKeys;

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

/// The application host's read of the status of its environment
/// (docs/architecture/execution.md#routing-and-availability): its row of
/// `platform.environment`, as `wamn_app`, kept in the host's [auth
/// memory](crate::auth_cache).
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
    async fn inactive(&self, project_env_key: &str) -> anyhow::Result<bool> {
        self.postgres
            .environment_inactive(&self.project, project_env_key)
            .await
    }
}

/// The host's authentication authority for PAT, session and signed requests.
#[derive(Debug)]
pub struct PlatformRouteAuthenticator {
    postgres: Arc<crate::plugins::wamn_postgres::WamnPostgres>,
    facts: Arc<ConnectionFacts>,
    project: Box<str>,
    identity_reader: Option<sqlx::PgPool>,
    verifier: Option<SessionVerifier<IssuerKeys>>,
}

impl PlatformRouteAuthenticator {
    /// Bind the host's project and optional identity reader and session verifier.
    pub fn new(
        postgres: Arc<crate::plugins::wamn_postgres::WamnPostgres>,
        facts: Arc<ConnectionFacts>,
        project: impl Into<Box<str>>,
        identity: Option<(sqlx::PgPool, Option<SessionVerifier<IssuerKeys>>)>,
    ) -> Self {
        let (identity_reader, verifier) = match identity {
            Some((reader, verifier)) => {
                postgres.set_hierarchy_reader(reader.clone());
                (Some(reader), verifier)
            }
            None => (None, None),
        };
        Self {
            postgres,
            facts,
            project: project.into(),
            identity_reader,
            verifier,
        }
    }

    /// Construct route authority without connection credentials for interface tests.
    #[cfg(any(test, feature = "test-util"))]
    pub fn for_test(
        postgres: Arc<crate::plugins::wamn_postgres::WamnPostgres>,
        project: &str,
        identity_reader: Option<sqlx::PgPool>,
        verifier: Option<SessionVerifier<IssuerKeys>>,
    ) -> Self {
        let facts = Arc::new(ConnectionFacts::new(
            Arc::new(crate::connection_memory::ConnectionMemory::default()),
            wamn_broker::Publisher::default(),
            Arc::new(crate::connection_credentials::ConnectionCredentials::empty()),
            Arc::clone(&postgres),
            "test",
            project,
            "dev",
        ));
        Self::new(
            postgres,
            facts,
            project,
            identity_reader.map(|reader| (reader, verifier)),
        )
    }

    async fn authenticate_signature(
        &self,
        header: &str,
        credential: &str,
        headers: &[Header],
        body: &[u8],
    ) -> Result<uuid::Uuid, AuthRejection> {
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
        let facts = &self.facts;
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
        mac.verify_slice(&signature)
            .map_err(|_| signature_unauthorized())?;
        Ok(fact.service_principal_id)
    }

    /// `cookie_headers` is `Some` only when `token` arrived by the session
    /// cookie; those requests then pass the CSRF test. Its flag is whether
    /// the route requires the CSRF header, which a read route does not.
    async fn authenticate_session(
        &self,
        attachment_id: &str,
        token: &str,
        cookie_headers: Option<(&[Header], bool)>,
        project_env_key: &str,
        environment: &str,
    ) -> Result<AuthenticatedCaller, AuthRejection> {
        let verifier = self.verifier.as_ref().ok_or_else(unauthorized)?;
        let identity_reader = self.identity_reader.as_ref().ok_or_else(unauthorized)?;
        let session = verifier.verify(token).await.map_err(|_| unauthorized())?;
        if let Some((headers, requires_csrf)) = cookie_headers {
            require_csrf(requires_csrf, session.claims().csrf.as_deref(), headers)?;
        }
        let principal = session.claims().sub;
        let permissions = self
            .postgres
            .session_operation_permissions(
                &self.project,
                project_env_key,
                &session.claims().roles,
                &principal,
            )
            .instrument(tracing::info_span!("wamn.auth.permissions"))
            .await
            .map_err(|error| {
                if error
                    .downcast_ref::<super::wamn_postgres::SessionGrantRefusal>()
                    .is_some()
                {
                    unauthorized()
                } else {
                    tracing::warn!(error = %error, "route operation grants unavailable");
                    authentication_unavailable()
                }
            })?;
        let cache = self.postgres.auth_cache();
        if !cache.session_active(token) {
            let ticket = cache.identity_ticket();
            let active = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                wamn_platform_identity::session_token::session_is_active(
                    &mut *identity_reader
                        .acquire()
                        .await
                        .map_err(|_| authentication_unavailable())?,
                    session.claims(),
                    &self.project,
                    environment,
                ),
            )
            .await
            .map_err(|_| authentication_unavailable())?
            .map_err(|_| authentication_unavailable())?;
            if !active {
                return Err(unauthorized());
            }
            let deadline = u64::try_from(session.claims().exp)
                .ok()
                .and_then(|seconds| {
                    std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(seconds))
                })
                .ok_or_else(unauthorized)?;
            cache.keep_session_active(
                ticket,
                token,
                session.claims().sub.to_string().as_str(),
                deadline,
            );
        }
        // Permission I/O cannot extend the evidence that admitted this request.
        // Once returned, nested work retains this caller without reauthentication.
        // If the key evidence expired during that I/O, the same token is
        // verified once more on fresh keys, and a token past its own age still
        // refuses (wamn-co0p).
        if session.require_admission().is_err() {
            verifier.verify(token).await.map_err(|_| unauthorized())?;
        }
        let active_role = permissions.active_role();
        Ok(AuthenticatedCaller::new(
            attachment_id,
            session.claims().sub.to_string().as_str(),
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
            let principal = self
                .authenticate_signature(header, credential, request.headers, request.body)
                .await?;
            let project = &self.project;
            let id = principal.to_string();
            let principal = id
                .parse::<wamn_identity_protocol::PrincipalId>()
                .map_err(|_| signature_unauthorized())?;
            let grants = self
                .postgres
                .principal_grants(project, &request.target.project_env_key, &principal)
                .await
                .map_err(|_| authentication_unavailable())?;
            if !grants.active || grants.user {
                return Err(signature_unauthorized());
            }
            let permissions = self
                .postgres
                .user_operation_permissions(project, &request.target.project_env_key, &principal)
                .await
                .map_err(|_| authentication_unavailable())?;
            return Ok(Some(AuthenticatedCaller::new(
                request.attachment_id,
                id,
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
                    &target.project_env_key,
                    &target.environment,
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
            let cache = self.postgres.auth_cache();
            let principal = if let Some(principal) = cache.pat(token) {
                principal
            } else {
                let ticket = cache.identity_ticket();
                let principal = wamn_platform_identity::authenticate_route_pat(
                    &mut *identity_reader
                        .acquire()
                        .await
                        .map_err(|_| authentication_unavailable())?,
                    token,
                )
                .instrument(tracing::info_span!("wamn.auth.identity"))
                .await
                .map_err(|error| {
                    tracing::warn!(error = %error, "route PAT authentication unavailable");
                    authentication_unavailable()
                })?
                .ok_or_else(unauthorized)?;
                cache.keep_pat(ticket, token, &principal);
                principal
            };
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
                    let facts = &self.facts;
                    let active = facts
                        .connection(&connection)
                        .await
                        .map_err(|_| authentication_unavailable())?;
                    if !active.is_some_and(|fact| {
                        fact.status == ConnectionStatus::Active
                            && fact.service_principal_id.to_string()
                                == principal.id().to_string().as_str()
                    }) {
                        return Err(unauthorized());
                    }
                }
                PrincipalType::Service => {}
            }
            // A service receives its roles through `wamn-ctl grant-role`.
            let permissions = self
                .postgres
                .user_operation_permissions(&self.project, &target.project_env_key, principal.id())
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
                attachment_id,
                principal.id().to_string().as_str(),
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
/// Only a browser login session of the host's control audience passes. Every
/// request tests again, through the `control` login, that the login is live and
/// that its principal still holds a control role in the token's org, so a
/// revoked role takes effect on the next request. The caller holds no
/// application role and no operation grant.
pub struct ControlRouteAuthenticator {
    verifier: SessionVerifier<IssuerKeys>,
    control: sqlx::PgPool,
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
    pub fn new(verifier: SessionVerifier<IssuerKeys>, control: sqlx::PgPool) -> Self {
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
            require_csrf(
                requires_csrf,
                session.claims().csrf.as_deref(),
                request.headers,
            )?;
        }
        let active = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            wamn_platform_identity::control::control_session_is_active(
                &mut *self
                    .control
                    .acquire()
                    .await
                    .map_err(|_| authentication_unavailable())?,
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
        if session.require_admission().is_err() {
            self.verifier
                .verify(token)
                .await
                .map_err(|_| unauthorized())?;
        }
        Ok(Some(AuthenticatedCaller::new(
            request.attachment_id,
            session.claims().sub.to_string().as_str(),
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
        AdmittedComponentOperation, AttachmentTarget, AttachmentType, DefinitionHash,
        PAT_AUTHENTICATION_MODE, PackageCoordinate, ServingAttachment, ServingComponent,
        ServingManifest, ServingRelease,
    };
    use wamn_engine::flow_http_routing::FlowHttpRouting;
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
                descriptor: {
                    let mut descriptor = wamn_catalog::ComponentDescriptor::named(
                        "http-request",
                        "0.1",
                        digest('a'),
                    );
                    descriptor.operations = BTreeMap::from([(
                        "request".into(),
                        AdmittedComponentOperation {
                            host_transaction: false,
                            input_ports: Vec::new(),
                            output_ports: Vec::new(),
                            parameters: Vec::new(),
                            committed_result_schema: None,
                            registered_operation: None,
                            statements: BTreeMap::new(),
                        },
                    )]);
                    descriptor
                },
            }]),
            BTreeSet::from([wamn_catalog::ServingRoute {
                package_id: "cat".into(),
                component: "http-request".into(),
                operation: "request".into(),
                type_: wamn_catalog::OperationType::Command,
                reads: BTreeSet::new(),
                revision: None,
                idempotency: None,
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
            wamn_engine::release_manifest::ReleaseTarget::new(
                wamn_registry::project_env_key("acme", "host-project", "dev"),
                "dev",
            )
            .with_applications(
                [(
                    "catalog".to_owned(),
                    wamn_catalog::ApplicationDefinition {
                        connections: std::collections::BTreeMap::new(),
                        packages: ["cat".to_owned()].into(),
                    },
                )]
                .into(),
            ),
        )
        .expect("fixture manifest loads");
        FlowHttpRouting::new(Some(Arc::new(release))).with_authenticator(Arc::new(
            PlatformRouteAuthenticator::for_test(
                Arc::new(crate::plugins::wamn_postgres::WamnPostgres::with_provider(
                    Arc::new(
                        crate::plugins::wamn_postgres::StaticCredentialProvider::new(
                            std::collections::HashMap::new(),
                            None,
                        ),
                    ),
                )),
                "host-project",
                None,
                None,
            ),
        ))
    }

    mod connection_live;
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
