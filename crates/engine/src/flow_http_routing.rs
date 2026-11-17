//! Host identity interface for the separate HTTP route component.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::Value;
use wamn_catalog::{AttachmentAuthPolicy, parse_attachment_auth_policy};
use wamn_identity_protocol::PAT_TOKEN_PREFIX;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::{Accessor, Resource};
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::route_bindings::wamn::flow_http_routing::routing;
/// The request header and the refusal that cross a [`RouteAuthenticator`].
pub use crate::route_bindings::wamn::flow_http_routing::routing::{AuthRejection, Header};

pub const FLOW_HTTP_ROUTING_ID: &str = "wamn-flow-http-routing";

const UNAUTHORIZED_STATUS: u16 = 401;
const UNAUTHORIZED_CODE: &str = "unauthorized";
const AUTHENTICATION_UNAVAILABLE_STATUS: u16 = 503;
const AUTHENTICATION_UNAVAILABLE_CODE: &str = "authentication-unavailable";
/// 501, because the request is well formed and it is the *host* that lacks the
/// mechanism — the caller can do nothing to satisfy a policy nothing implements.
const UNSUPPORTED_POLICY_STATUS: u16 = 501;
const UNSUPPORTED_POLICY_CODE: &str = "auth-policy-unsupported";
/// 503, because the environment exists and serves again once it is active
/// (docs/architecture/execution.md#routing-and-availability).
const ENVIRONMENT_INACTIVE_STATUS: u16 = 503;
const ENVIRONMENT_INACTIVE_CODE: &str = "environment-inactive";

/// Host-owned record of an originating caller and its operation grants.
///
/// The grants are the stored application authority of the caller: whether it
/// holds `admin`, and the distinct stable operation references
/// (`<package>:<interface>/<operation>`) its other roles hold
/// (docs/architecture/execution.md#routing-and-availability).
///
/// The guest can hold only the resource handle. It cannot construct this value,
/// inspect the grant set, or replace the principal while forwarding it to router
/// delivery.
#[derive(Clone)]
pub struct AuthenticatedCaller {
    attachment_id: wamn_catalog::canonical_name::CanonicalName,
    principal_id: wamn_identity_protocol::PrincipalId,
    credential_type: CredentialType,
    admin: bool,
    permissions: Arc<HashSet<String>>,
    active_role: Option<Box<str>>,
}

/// The credential that authenticated the original request, owned by the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialType {
    /// A freshly authenticated platform access token.
    Pat,
    /// Verified bearer identity with project database grants.
    Session,
    /// A service identity pinned by trusted queue admission.
    QueuedService,
    /// A signature verified with an enabled external connection credential.
    Connection,
}

impl std::fmt::Debug for AuthenticatedCaller {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticatedCaller")
            .field("attachment_id", &self.attachment_id)
            .field("principal_id", &self.principal_id)
            .field("credential_type", &self.credential_type)
            .field("admin", &self.admin)
            .field("active_role", &self.active_role)
            .field("permission_count", &self.permissions.len())
            .finish_non_exhaustive()
    }
}

impl AuthenticatedCaller {
    /// Record the caller that a host authenticated for one attachment.
    ///
    /// `admin` is whether the caller currently holds the built-in `admin` role.
    /// `permissions` are the stable operation references of its other roles.
    /// `active_role` is the one role the caller acts with, when it has one
    /// (docs/architecture/execution.md#environment-workflows).
    pub fn new(
        attachment_id: wamn_catalog::canonical_name::CanonicalName,
        principal_id: wamn_identity_protocol::PrincipalId,
        credential_type: CredentialType,
        admin: bool,
        permissions: HashSet<String>,
        active_role: Option<Box<str>>,
    ) -> Self {
        Self {
            attachment_id,
            principal_id,
            credential_type,
            admin,
            permissions: Arc::new(permissions),
            active_role,
        }
    }

    /// Return the immutable attachment identity whose policy produced this caller record.
    pub fn attachment_id(&self) -> &wamn_catalog::canonical_name::CanonicalName {
        &self.attachment_id
    }

    /// Return the opaque platform principal used by router traces and refusals.
    pub fn principal_id(&self) -> &wamn_identity_protocol::PrincipalId {
        &self.principal_id
    }

    /// Return the original credential type, unchanged across nested calls.
    pub fn credential_type(&self) -> CredentialType {
        self.credential_type
    }

    /// Whether the caller holds the built-in `admin` role.
    pub fn admin(&self) -> bool {
        self.admin
    }

    /// The one role the caller acts with: `admin` for an admin, the single
    /// role of a caller that holds exactly one, the run's role for a queued
    /// run, and none otherwise.
    pub fn active_role(&self) -> Option<&str> {
        self.active_role.as_deref()
    }

    /// Make sure of one sealed registered-operation token of the serving release.
    ///
    /// `admin` holds every operation the release serves. Any other caller holds
    /// the operation when it holds the token's stable reference, the token
    /// without its `@<version>`. The caller asks only about tokens of the
    /// serving release, so a stored reference the release does not serve grants
    /// nothing. A host route is not grantable, so a stored row that names one
    /// grants nothing either (docs/architecture/execution.md#host-run-routes),
    /// except a granted host route such as `number_counter.set`
    /// (docs/architecture/data-access.md#setting-a-counter).
    pub fn permits(&self, operation: &str) -> bool {
        self.admin
            || ((!wamn_catalog::is_host_route_operation(operation)
                || wamn_catalog::is_granted_host_route_operation(operation))
                && self
                    .permissions
                    .contains(wamn_catalog::sealed_operation_reference(operation)))
    }
}

/// Authentication facts resolved by the host for one route.
#[derive(Debug, Clone, Copy)]
pub struct RoutePolicy<'a> {
    pub policy: &'a Value,
}

/// The host's route policies, supplied by workload config or an edge bundle.
pub trait RouteProvider: Send + Sync + std::fmt::Debug {
    fn target(&self) -> &wamn_registry::ProjectEnvKey;
    fn route(&self, id: &wamn_catalog::canonical_name::CanonicalName) -> Option<RoutePolicy<'_>>;
}

/// The host's mechanism for a route whose policy names a credential.
///
/// The route plugin resolves the attachment and its policy, and it admits a
/// `none` policy itself. Every other policy reaches the authenticator. It reads
/// the credential from the headers and returns the caller with its exact
/// operation grants, no caller for a verified signature, or an HTTP refusal.
#[async_trait::async_trait]
pub trait RouteAuthenticator: Send + Sync + std::fmt::Debug {
    /// Authenticate one request to a route whose policy is not `none`.
    async fn authenticate(
        &self,
        request: AuthenticationRequest<'_>,
    ) -> Result<Option<AuthenticatedCaller>, AuthRejection>;
}

/// The host's read of whether its environment is inactive.
///
/// The route plugin asks it once for each request, before the route's policy,
/// so an anonymous route is refused too. An inactive environment answers
/// `environment-inactive` with 503. A failed read answers
/// `authentication-unavailable`.
#[async_trait::async_trait]
pub trait EnvironmentStatus: Send + Sync + std::fmt::Debug {
    /// Whether the environment of `project_env_key` is inactive.
    async fn inactive(
        &self,
        project_env_key: &wamn_registry::ProjectEnvKey,
    ) -> anyhow::Result<bool>;
}

/// One request to a protected route, as the route plugin resolved it.
#[derive(Clone, Copy)]
pub struct AuthenticationRequest<'a> {
    /// The project and environment of the host that serves the route.
    pub target: &'a wamn_registry::ProjectEnvKey,
    pub attachment_id: &'a wamn_catalog::canonical_name::CanonicalName,
    /// The attachment's parsed policy. It is never `none`.
    pub policy: AttachmentAuthPolicy<'a>,
    pub headers: &'a [Header],
    /// Exact request bytes, before JSON decoding or route mapping.
    pub body: &'a [u8],
}

/// Hand-written so a debug print never shows a header, because the headers
/// carry the credential.
impl std::fmt::Debug for AuthenticationRequest<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticationRequest")
            .field("attachment_id", &self.attachment_id)
            .field("policy", &self.policy)
            .field("header_count", &self.headers.len())
            .finish_non_exhaustive()
    }
}

/// Authoritative HTTP route supply for the flow-http adapter.
pub struct FlowHttpRouting {
    scopes: Arc<crate::invocation_scope::InvocationScopes>,
    authenticator: Option<Arc<dyn RouteAuthenticator>>,
    environment: Option<Arc<dyn EnvironmentStatus>>,
}
impl std::fmt::Debug for FlowHttpRouting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlowHttpRouting").finish_non_exhaustive()
    }
}
impl FlowHttpRouting {
    pub fn new(scopes: Arc<crate::invocation_scope::InvocationScopes>) -> Self {
        Self {
            scopes,
            authenticator: None,
            environment: None,
        }
    }
    /// Refuse every route while the host's environment is inactive.
    #[must_use]
    pub fn with_environment_status(mut self, environment: Arc<dyn EnvironmentStatus>) -> Self {
        self.environment = Some(environment);
        self
    }

    /// Hand every protected route to the host's credential mechanism.
    #[must_use]
    pub fn with_authenticator(mut self, authenticator: Arc<dyn RouteAuthenticator>) -> Self {
        self.authenticator = Some(authenticator);
        self
    }

    /// Apply a host-resolved policy, including a fixed native host route.
    pub async fn authenticate_policy(
        &self,
        target: &wamn_registry::ProjectEnvKey,
        attachment_id: &wamn_catalog::canonical_name::CanonicalName,
        route: RoutePolicy<'_>,
        headers: &[Header],
        body: &[u8],
    ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
        if let Some(environment) = &self.environment {
            let inactive = environment.inactive(target).await.map_err(|error| {
                tracing::warn!(error = %error, "environment status unavailable");
                authentication_unavailable()
            })?;
            if inactive {
                return Err(AuthRejection {
                    status: ENVIRONMENT_INACTIVE_STATUS,
                    code: ENVIRONMENT_INACTIVE_CODE.to_owned(),
                });
            }
        }
        let policy = parse_attachment_auth_policy(route.policy).ok_or_else(|| AuthRejection {
            status: UNSUPPORTED_POLICY_STATUS,
            code: UNSUPPORTED_POLICY_CODE.to_owned(),
        })?;
        if policy == AttachmentAuthPolicy::None {
            return Ok(None);
        }
        let authenticator = self.authenticator.as_ref().ok_or_else(|| {
            if matches!(policy, AttachmentAuthPolicy::Signature { .. }) {
                signature_unauthorized()
            } else {
                authentication_unavailable()
            }
        })?;
        authenticator
            .authenticate(AuthenticationRequest {
                target,
                attachment_id,
                policy,
                headers,
                body,
            })
            .await
    }

    async fn authenticate_accepted(
        &self,
        routes: &dyn RouteProvider,
        attachment_id: &wamn_catalog::canonical_name::CanonicalName,
        accepted: &Value,
        headers: &[Header],
        body: &[u8],
    ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
        let route = routes
            .route(attachment_id)
            .ok_or_else(authentication_unavailable)?;
        if route.policy != accepted {
            return Err(authentication_unavailable());
        }
        self.authenticate_policy(routes.target(), attachment_id, route, headers, body)
            .await
    }
}

#[derive(Clone, Copy)]
pub enum RouteCredential<'a> {
    /// An RFC 9068 access token.
    Session(&'a str),
    /// A personal access token.
    Pat(&'a str),
}

/// Hand-written so a debug print never shows the token.
impl std::fmt::Debug for RouteCredential<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Session(_) => formatter.write_str("Session"),
            Self::Pat(_) => formatter.write_str("Pat"),
        }
    }
}

/// Select one bearer credential from the route policy and the PAT prefix.
pub fn route_credential<'a>(
    request: &AuthenticationRequest<'a>,
) -> Result<RouteCredential<'a>, AuthRejection> {
    let policy = request.policy;
    if matches!(policy, AttachmentAuthPolicy::Signature { .. }) {
        return Err(signature_unauthorized());
    }
    let token = required_bearer_token(request.headers)?;
    if policy.allows_pat() && (!policy.allows_session() || token.starts_with(PAT_TOKEN_PREFIX)) {
        return Ok(RouteCredential::Pat(token));
    }
    if policy.allows_session() {
        return Ok(RouteCredential::Session(token));
    }
    Err(unauthorized())
}

/// Extract one standard bearer presentation without exposing why it failed.
pub fn bearer_token(headers: &[Header]) -> Option<&str> {
    let mut values = headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("authorization"));
    let value = values.next()?.value.as_str();
    if values.next().is_some() {
        return None;
    }
    let mut fields = value.split_ascii_whitespace();
    let scheme = fields.next()?;
    let token = fields.next()?;
    (scheme.eq_ignore_ascii_case("bearer") && fields.next().is_none()).then_some(token)
}

/// The one standard bearer token, or the opaque refusal.
pub fn required_bearer_token(headers: &[Header]) -> Result<&str, AuthRejection> {
    bearer_token(headers).ok_or_else(unauthorized)
}

/// The refusal for a missing, malformed or rejected credential.
pub fn unauthorized() -> AuthRejection {
    AuthRejection {
        status: UNAUTHORIZED_STATUS,
        code: UNAUTHORIZED_CODE.to_string(),
    }
}

/// Refuse a signature without exposing any detail in the HTTP body.
pub fn signature_unauthorized() -> AuthRejection {
    AuthRejection {
        status: UNAUTHORIZED_STATUS,
        code: String::new(),
    }
}

/// The refusal for a credential mechanism that cannot answer now.
pub fn authentication_unavailable() -> AuthRejection {
    AuthRejection {
        status: AUTHENTICATION_UNAVAILABLE_STATUS,
        code: AUTHENTICATION_UNAVAILABLE_CODE.to_string(),
    }
}

#[async_trait::async_trait]
impl HostPlugin for FlowHttpRouting {
    fn id(&self) -> &'static str {
        FLOW_HTTP_ROUTING_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:flow-http-routing/routing@0.1.0")]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_unbind(&self, workload: &str, _: WitInterfaces<'_>) -> anyhow::Result<()> {
        self.scopes.revoke_workload(workload);
        Ok(())
    }

    /// `world` only decides whether this callback fires; the linker entry is made
    /// here or the guest's import resolves to nothing.
    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if !interfaces.contains("wamn", "flow-http-routing", &["routing"]) {
            return Ok(());
        }
        if let Some(routes) =
            crate::application::WorkloadRoutes::read(&item.local_resources().config)?
        {
            self.scopes.bind_routes(item.id(), Arc::new(routes));
            self.scopes.bind_workload(item.id(), item.workload_id());
        }
        add_to_linker(item.linker()?)?;
        Ok(())
    }
}

/// Link the routing import. A store without this plugin traps on each call.
pub fn add_to_linker(
    linker: &mut wash_runtime::wasmtime::component::Linker<SharedCtx>,
) -> wash_runtime::wasmtime::Result<()> {
    routing::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<FlowHttpRouting>> {
    ctx.try_get_plugin::<FlowHttpRouting>(FLOW_HTTP_ROUTING_ID)
}

impl<T: 'static + Send> routing::HostWithStore<T> for SharedCtx {
    async fn authenticate(
        accessor: &Accessor<T, Self>,
        attachment_id: String,
        accepted: String,
        headers: Vec<Header>,
        body: Vec<u8>,
    ) -> wash_runtime::wasmtime::Result<Result<Option<Resource<AuthenticatedCaller>>, AuthRejection>>
    {
        let Ok(attachment_id) =
            attachment_id.parse::<wamn_catalog::canonical_name::CanonicalName>()
        else {
            return Ok(Err(authentication_unavailable()));
        };
        let (plugin, release, trace) = accessor.with(|mut access| {
            let ctx = access.get();
            let trace = crate::invocation_trace::request_trace(
                &headers
                    .iter()
                    .map(|header| (header.name.clone(), header.value.clone()))
                    .collect::<Vec<_>>(),
            );
            let plugin = plugin_of(&ctx)?;
            let release = plugin.scopes.routes(&ctx.component_id);
            if release.is_some() {
                let lifetime = plugin
                    .scopes
                    .enter_trace(ctx.id.to_string(), trace.clone())
                    .map_err(|error| wash_runtime::wasmtime::Error::msg(error.to_string()))?;
                ctx.table.push(lifetime)?;
            }
            Ok::<_, wash_runtime::wasmtime::Error>((plugin, release, trace))
        })?;
        trace
            .run(async move {
                let Some(release) = release else {
                    return Ok(Err(authentication_unavailable()));
                };
                let accepted: Value = serde_json::from_str(&accepted)?;
                let caller = match plugin
                    .authenticate_accepted(
                        release.as_ref(),
                        &attachment_id,
                        &accepted,
                        &headers,
                        &body,
                    )
                    .await
                {
                    Ok(caller) => caller,
                    Err(rejection) => return Ok(Err(rejection)),
                };
                accessor.with(|mut access| {
                    Ok(Ok(caller
                        .map(|caller| access.get().table.push(caller))
                        .transpose()?))
                })
            })
            .await
    }
}

impl routing::Host for ActiveCtx<'_> {}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "`routing::HostAuthenticatedCaller` is generated by wit-bindgen and declares \
              `async fn`; dropping a caller awaits nothing but still has to match it"
)]
impl routing::HostAuthenticatedCaller for ActiveCtx<'_> {
    async fn drop(
        &mut self,
        caller: Resource<AuthenticatedCaller>,
    ) -> wash_runtime::wasmtime::Result<()> {
        self.table.delete(caller)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bearer_parsing_has_one_success_shape_and_one_opaque_refusal_class() {
        let header = |name: &str, value: &str| Header {
            name: name.to_string(),
            value: value.to_string(),
        };
        assert_eq!(
            bearer_token(&[header("Authorization", "bEaReR secret")]),
            Some("secret")
        );
        for refused in [
            vec![],
            vec![header("authorization", "Basic secret")],
            vec![header("authorization", "Bearer")],
            vec![header("authorization", "Bearer secret extra")],
            vec![
                header("authorization", "Bearer first"),
                header("Authorization", "Bearer second"),
            ],
        ] {
            let rejection = required_bearer_token(&refused)
                .expect_err("every malformed bearer presentation refuses");
            assert_eq!(rejection.status, UNAUTHORIZED_STATUS);
            assert_eq!(rejection.code, UNAUTHORIZED_CODE);
        }
    }

    fn header(name: &str, value: &str) -> Header {
        Header {
            name: name.to_string(),
            value: value.to_string(),
        }
    }

    fn assert_unauthorized(rejection: &AuthRejection) {
        assert_eq!(rejection.status, UNAUTHORIZED_STATUS);
        assert_eq!(rejection.code, UNAUTHORIZED_CODE);
    }

    #[test]
    fn originating_caller_holds_its_references_or_admin() {
        let caller = AuthenticatedCaller {
            attachment_id: "widget-http".parse().unwrap(),
            principal_id: "11111111-1111-4111-8111-111111111111".parse().unwrap(),
            credential_type: CredentialType::Pat,
            admin: false,
            permissions: Arc::new(HashSet::from(["platform-fixture:widget/get".to_string()])),
            active_role: None,
        };
        assert_eq!(caller.attachment_id(), "widget-http");
        assert_eq!(
            caller.principal_id(),
            &"11111111-1111-4111-8111-111111111111"
                .parse::<wamn_identity_protocol::PrincipalId>()
                .unwrap()
        );
        // A stored reference holds the operation at any served version.
        assert!(caller.permits("platform-fixture:widget/get@1.0.0"));
        assert!(caller.permits("platform-fixture:widget/get@2.1.0"));
        assert!(!caller.permits("platform-fixture:widget/query@1.0.0"));
        assert!(!caller.permits("platform-fixture:widget-maker/get@1.0.0"));
        assert!(!caller.permits("widget.get"));
        let admin = AuthenticatedCaller {
            admin: true,
            permissions: Arc::new(HashSet::new()),
            ..caller.clone()
        };
        assert!(admin.permits("platform-fixture:widget/query@1.0.0"));
        assert!(admin.permits("wamn-control:role/create@0.9.0"));
        // A stored row that names a host route admits no caller but admin.
        let stored = AuthenticatedCaller {
            permissions: Arc::new(HashSet::from(["wamn-control:role/create".to_string()])),
            ..caller.clone()
        };
        assert!(!stored.permits("wamn-control:role/create@0.9.0"));
        assert_eq!(
            wamn_catalog::sealed_operation_reference("platform-fixture:widget/get@1.0.0"),
            "platform-fixture:widget/get"
        );
        assert_eq!(caller.credential_type(), CredentialType::Pat);
        for type_ in [CredentialType::Pat, CredentialType::Session] {
            let caller = AuthenticatedCaller {
                credential_type: type_,
                ..caller.clone()
            };
            let nested = caller.clone();
            assert_eq!(nested.credential_type(), type_);
            assert_eq!(nested.principal_id(), caller.principal_id());
            assert!(nested.permits("platform-fixture:widget/get@1.0.0"));
        }
    }
}
