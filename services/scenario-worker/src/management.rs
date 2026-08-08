//! Authenticated management transport for the canonical authoring commands.
//!
//! This is the boundary `authoring.rs` deferred until item 5 owned retained
//! client identity. It verifies a personal-access-token presenter against the T1
//! system database, derives trusted principal and project-role context, records
//! that principal on an append-only command ledger, and only then invokes the
//! internal adapter. The adapter itself stays principal-free: it keeps its
//! process-local capability token and learns who the caller was only through the
//! ledger row this module writes.
//!
//! Two presenters reach the same authority. A personal access token serves
//! headless callers; a browser session serves the console. Both resolve through
//! [`role_for`] into one [`AuthorizedAuthor`], so nothing downstream can tell
//! them apart and neither can widen what the other may do. A session is the
//! weaker position — the browser replays its cookie automatically — so a
//! session-presented state change additionally carries the synchronizer token
//! bound to its session row, and is refused without it.
//!
//! Trusted context never comes from the request. [`AuthorizedAuthor`] has no
//! public constructor and implements no deserialization trait, exactly like the
//! [`AuthenticatedPrincipal`] it is derived from, and no handler here reads any
//! header other than the three that carry a credential — `authorization`,
//! `cookie`, and the CSRF header — or any body field naming an identity.

use std::convert::Infallible;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio_postgres::{Client, GenericClient, NoTls};

use wamn_authoring_model::{
    AuthoringCommand, AuthoringCommandKind, AuthoringDocument, AuthoringOutcome, AuthoringRefusal,
    AuthoringRequest, AuthoringResponse, AuthoringSuccess, CommandRefusal, CommitProvenance,
    ContractDecodeError, DraftIdentity, SCHEMA_VERSION, SafeUint64, decode_document,
};
use wamn_platform_identity::{
    AuthenticatedPrincipal, IdentityErrorKind, IssuedSession, PrincipalKind, ProjectRole,
    authenticate_pat, authenticate_session, login_local, login_session, project_roles,
    revoke_session,
};

use crate::authoring::{
    DraftRunRefusal, GrantDraftSafeGeneration, InternalAuthoringBackend, RevokeDraftSafeGeneration,
    RevokeDraftSafeGenerationResult, SaveFlowDraft, SaveFlowDraftResult, ValidateFlowDraft,
    ValidatedDraftPin,
};

/// The append-only ledger row every authorized management command writes.
///
/// The principal columns are denormalized text, not a foreign key: principals
/// live in the T1 system database while this ledger lives in the project
/// database, so a row has to stand on its own.
/// `wamn_run.effect_disposition_requests` carries the same shape for the same
/// reason.
const INSERT_COMMAND_AUDIT_SQL: &str = "INSERT INTO catalog.authoring_command_audit \
    (tenant_id, command_id, command_kind, principal_id, principal_kind, \
     principal_subject, effective_role, org, project, environment, target_ref, \
     provenance_commit, provenance_ref, provenance_dirty) \
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)";

/// Bearer scheme this surface accepts, including its single trailing space.
const BEARER_SCHEME: &str = "Bearer ";

/// Name of the cookie carrying the browser session value.
const SESSION_COOKIE_NAME: &str = "wamn_session";

/// Header carrying the synchronizer token bound to the presented session.
///
/// It is a header rather than a body field on purpose: a cross-site form post
/// can be made to carry the ambient cookie, but it cannot set a custom header,
/// and it cannot read the login response body the token was handed out in.
const CSRF_HEADER: &str = "x-wamn-csrf";

/// Attributes every session cookie carries. They are the browser-side half of
/// the defence and are deliberately not configurable: `HttpOnly` keeps page
/// script — and therefore any XSS — away from the value, `SameSite=Strict` stops
/// a cross-site context from sending it at all, `Secure` keeps it off plaintext
/// transports, and `Path=/` scopes it to this surface.
const SESSION_COOKIE_ATTRIBUTES: &str = "; HttpOnly; SameSite=Strict; Secure; Path=/";

/// Roles this boundary admits for the authoring command surface.
///
/// Identity storage keeps role slugs opaque on purpose; attaching permission
/// meaning is the management boundary's job, so the vocabulary lives here and in
/// the ledger's `effective_role` CHECK.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ManagementRole {
    /// May run the authoring commands for one project.
    ProjectAuthor,
    /// Everything an author may do; ordered above it for role selection.
    ProjectAdmin,
}

impl ManagementRole {
    /// Return the stable role slug shared by identity storage and the ledger.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProjectAuthor => "project-author",
            Self::ProjectAdmin => "project-admin",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "project-author" => Some(Self::ProjectAuthor),
            "project-admin" => Some(Self::ProjectAdmin),
            _ => None,
        }
    }
}

impl fmt::Display for ManagementRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Every command the ledger can attribute.
///
/// A superset of the client contract inventory: the two connection-generation
/// mutations are host-side operator actions with no client command, but they are
/// canonical authoring mutations and are attributed like the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditedCommand {
    SaveFlowDraft,
    Validate,
    DraftRun,
    SuiteRun,
    Publish,
    SuiteProjection,
    GrantDraftSafeGeneration,
    RevokeDraftSafeGeneration,
}

impl AuditedCommand {
    /// Return the stable ledger literal. The contract kinds keep exactly the
    /// spelling the wire contract uses; a unit test pins them to `serde`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SaveFlowDraft => "save-flow-draft",
            Self::Validate => "validate",
            Self::DraftRun => "draft-run",
            Self::SuiteRun => "suite-run",
            Self::Publish => "publish",
            Self::SuiteProjection => "suite-projection",
            Self::GrantDraftSafeGeneration => "grant-draft-safe-generation",
            Self::RevokeDraftSafeGeneration => "revoke-draft-safe-generation",
        }
    }
}

impl fmt::Display for AuditedCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Trusted authoring context derived only from a verified presenter.
///
/// Produced exclusively by [`authorize`] from an [`AuthenticatedPrincipal`] and
/// a stored project role. It has no public constructor and implements no
/// deserialization trait, so no request body field and no request header can
/// turn client input into one.
///
/// ```compile_fail
/// use wamn_scenario_worker::management::AuthorizedAuthor;
/// let forged = AuthorizedAuthor { subject: "someone-else".into() };
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedAuthor {
    principal_id: Box<str>,
    principal_kind: PrincipalKind,
    subject: Box<str>,
    role: ManagementRole,
}

impl AuthorizedAuthor {
    /// Return the opaque T1 principal ID recorded on every ledger row.
    pub fn principal_id(&self) -> &str {
        &self.principal_id
    }

    /// Return whether a human or a service presented the token.
    pub const fn principal_kind(&self) -> PrincipalKind {
        self.principal_kind
    }

    /// Return the verified first-party subject.
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Return the role this principal exercised for the selected project.
    pub const fn role(&self) -> ManagementRole {
        self.role
    }
}

/// The project scope one command runs in, already reconciled with the fixed
/// scope the adapter was started for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandScope {
    tenant_id: Box<str>,
    org: Box<str>,
    project: Box<str>,
    environment: Box<str>,
}

impl CommandScope {
    /// Build the scope an in-process caller runs commands under.
    ///
    /// The HTTP surface derives its scope from the fixed configuration instead;
    /// this constructor exists for callers that already hold trusted scope.
    pub fn new(tenant_id: &str, org: &str, project: &str, environment: &str) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            org: org.into(),
            project: project.into(),
            environment: environment.into(),
        }
    }
}

/// One authorized management command, recorded before the command runs.
///
/// Construction takes an [`AuthorizedAuthor`], so an attributed row cannot be
/// built without a verified presenter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandAudit {
    scope: CommandScope,
    command_id: Box<str>,
    command: AuditedCommand,
    author: AuthorizedAuthor,
    target_ref: Box<str>,
    /// The client's own claim about where it read the content, retained
    /// verbatim beside the principal that was actually verified. It is written,
    /// never read: nothing on this path branches on it.
    provenance: Option<CommitProvenance>,
}

impl CommandAudit {
    /// Return the tenant whose ledger this row belongs to.
    pub fn tenant_id(&self) -> &str {
        &self.scope.tenant_id
    }
}

/// Verify a bearer token and resolve the role it may exercise for one project.
///
/// [`authorize_session`] is the same function for the browser presenter; both
/// end in [`role_for`].
///
/// Malformed, unknown, forged, expired, and revoked tokens, disabled principals,
/// principals with no role in this project, and principals whose roles all fall
/// outside [`ManagementRole`] every return `Ok(None)`. Only infrastructure
/// failure is an `Err`, so a transport may answer every refusal with one
/// response without leaking which predicate failed.
pub async fn authorize(
    system_client: &(impl GenericClient + Sync),
    token: &str,
    org: &str,
    project: &str,
) -> anyhow::Result<Option<AuthorizedAuthor>> {
    let Some(authenticated) = authenticate_pat(system_client, token).await? else {
        return Ok(None);
    };
    role_for(system_client, &authenticated, org, project).await
}

/// Verify a session cookie and its CSRF proof, and resolve the role it may
/// exercise for one project.
///
/// The cookie and the proof are verified together by the identity core, so a
/// request that presents a valid cookie without the matching synchronizer token
/// is refused here — before this function returns, and therefore before any
/// caller can read a body or reach a command. Every refusal [`authorize`] can
/// produce, plus a missing or wrong CSRF proof, returns `Ok(None)`.
pub async fn authorize_session(
    system_client: &(impl GenericClient + Sync),
    cookie: &str,
    csrf_token: &str,
    org: &str,
    project: &str,
) -> anyhow::Result<Option<AuthorizedAuthor>> {
    let Some(authenticated) = authenticate_session(system_client, cookie, csrf_token).await? else {
        return Ok(None);
    };
    role_for(system_client, &authenticated, org, project).await
}

/// Resolve the role an already-authenticated principal may exercise.
///
/// Both presenters funnel through here. That is the whole reason a session is a
/// presenter and not an authority: the roles come from the same
/// `identity.project_roles` rows and the same [`admitted_role`] selection, so a
/// session and a token held by one principal cannot resolve differently.
async fn role_for(
    system_client: &(impl GenericClient + Sync),
    authenticated: &AuthenticatedPrincipal,
    org: &str,
    project: &str,
) -> anyhow::Result<Option<AuthorizedAuthor>> {
    let roles = project_roles(system_client, authenticated.principal().id(), org, project).await?;
    let Some(role) = admitted_role(&roles) else {
        return Ok(None);
    };
    Ok(Some(author_from_authenticated(authenticated, role)))
}

/// Return the strongest admitted role, ignoring slugs this boundary attaches no
/// meaning to.
fn admitted_role(roles: &[ProjectRole]) -> Option<ManagementRole> {
    roles
        .iter()
        .filter_map(|role| ManagementRole::parse(role.as_str()))
        .max()
}

fn author_from_authenticated(
    authenticated: &AuthenticatedPrincipal,
    role: ManagementRole,
) -> AuthorizedAuthor {
    let principal = authenticated.principal();
    AuthorizedAuthor {
        principal_id: principal.id().as_str().into(),
        principal_kind: principal.kind(),
        subject: principal.subject().into(),
        role,
    }
}

/// Write one attributed ledger row on an already-scoped author credential.
pub(crate) async fn insert_command_audit(
    client: &Client,
    audit: &CommandAudit,
) -> anyhow::Result<()> {
    client
        .execute(
            INSERT_COMMAND_AUDIT_SQL,
            &[
                &audit.scope.tenant_id.as_ref(),
                &audit.command_id.as_ref(),
                &audit.command.as_str(),
                &audit.author.principal_id.as_ref(),
                &audit.author.principal_kind.as_str(),
                &audit.author.subject.as_ref(),
                &audit.author.role.as_str(),
                &audit.scope.org.as_ref(),
                &audit.scope.project.as_ref(),
                &audit.scope.environment.as_ref(),
                &audit.target_ref.as_ref(),
                &audit
                    .provenance
                    .as_ref()
                    .map(|source| source.commit.as_str()),
                &audit
                    .provenance
                    .as_ref()
                    .and_then(|source| source.r#ref.as_deref()),
                &audit.provenance.as_ref().map(|source| source.dirty),
            ],
        )
        .await
        .context("record authoring command audit")?;
    Ok(())
}

/// Record the attributed ledger row for one command about to run.
///
/// The row is written before the command, so the ledger retains every
/// authorized attempt rather than only the attempts that happened to succeed —
/// the posture `wamn_run.effect_disposition_requests` takes for the same reason.
async fn record(
    backend: &mut InternalAuthoringBackend,
    author: &AuthorizedAuthor,
    scope: &CommandScope,
    command_id: &str,
    command: AuditedCommand,
    target_ref: &str,
    provenance: Option<&CommitProvenance>,
) -> anyhow::Result<()> {
    let audit = CommandAudit {
        scope: scope.clone(),
        command_id: command_id.into(),
        command,
        author: author.clone(),
        target_ref: target_ref.into(),
        provenance: provenance.cloned(),
    };
    backend.record_command_audit(&audit).await
}

/// Save one flow draft, attributing the save to its verified author.
///
/// `provenance` is the client's optional claim about where it read the content.
/// It reaches the ledger and stops there: this function does not branch on it,
/// does not pass it to the command, and produces the identical result whether it
/// is present or absent.
pub async fn save_flow_draft(
    backend: &mut InternalAuthoringBackend,
    author: &AuthorizedAuthor,
    scope: &CommandScope,
    command_id: &str,
    request: &SaveFlowDraft,
    provenance: Option<&CommitProvenance>,
) -> anyhow::Result<SaveFlowDraftResult> {
    record(
        backend,
        author,
        scope,
        command_id,
        AuditedCommand::SaveFlowDraft,
        &request.draft_id,
        provenance,
    )
    .await?;
    backend.save_flow_draft(request).await
}

/// Validate one exact draft revision, attributing it to its verified author.
pub async fn validate_flow_draft(
    backend: &mut InternalAuthoringBackend,
    author: &AuthorizedAuthor,
    scope: &CommandScope,
    command_id: &str,
    request: &ValidateFlowDraft,
    flowrunner_bytes: &[u8],
) -> anyhow::Result<Result<ValidatedDraftPin, DraftRunRefusal>> {
    record(
        backend,
        author,
        scope,
        command_id,
        AuditedCommand::Validate,
        &request.draft_id,
        None,
    )
    .await?;
    backend.validate_flow_draft(request, flowrunner_bytes).await
}

/// Grant one draft-safe connection generation, attributing it to its author.
pub async fn grant_draft_safe_generation(
    backend: &mut InternalAuthoringBackend,
    author: &AuthorizedAuthor,
    scope: &CommandScope,
    command_id: &str,
    grant: &GrantDraftSafeGeneration,
) -> anyhow::Result<()> {
    record(
        backend,
        author,
        scope,
        command_id,
        AuditedCommand::GrantDraftSafeGeneration,
        &grant.instance_id,
        None,
    )
    .await?;
    backend.grant_draft_safe_generation(grant).await
}

/// Revoke one draft-safe connection generation, attributing it to its author.
pub async fn revoke_draft_safe_generation(
    backend: &mut InternalAuthoringBackend,
    author: &AuthorizedAuthor,
    scope: &CommandScope,
    command_id: &str,
    revoke: &RevokeDraftSafeGeneration,
) -> anyhow::Result<RevokeDraftSafeGenerationResult> {
    record(
        backend,
        author,
        scope,
        command_id,
        AuditedCommand::RevokeDraftSafeGeneration,
        &revoke.instance_id,
        None,
    )
    .await?;
    backend.revoke_draft_safe_generation(revoke).await
}

/// Arguments for the authenticated management authoring listener.
#[derive(Clone, Debug, clap::Args)]
pub struct ManagementServeArgs {
    /// Address the management authoring surface listens on.
    #[arg(
        long = "bind",
        env = "WAMN_MANAGEMENT_BIND",
        default_value = "0.0.0.0:8088"
    )]
    pub bind: String,

    /// T1 system database holding first-party principals, tokens, and roles.
    #[arg(long = "system-url", env = "WAMN_SYSTEM_URL")]
    pub system_url: String,

    /// Dedicated host-author credential for the project database.
    #[arg(long = "authoring-database-url", env = "WAMN_AUTHORING_PG_URL")]
    pub authoring_database_url: String,

    /// Organization whose project roles admit a caller.
    #[arg(long, env = "WAMN_MANAGEMENT_ORG")]
    pub org: String,

    /// The single project this surface serves.
    #[arg(long, env = "WAMN_MANAGEMENT_PROJECT")]
    pub project: String,

    /// Fixed authoring tenant the adapter is scoped to.
    #[arg(long, env = "WAMN_MANAGEMENT_TENANT")]
    pub tenant: String,

    /// Schema containing the stored flow and scenario catalog.
    #[arg(long, default_value = "wamn_run")]
    pub source_schema: String,

    /// Lifetime of a token minted by the reserved login route.
    #[arg(long = "login-token-ttl-secs", default_value_t = 3600)]
    pub login_token_ttl_secs: u64,

    /// Lifetime of a browser session opened by the reserved session route.
    /// Expiry is absolute: a session is never extended by being used.
    #[arg(long = "session-ttl-secs", default_value_t = 43200)]
    pub session_ttl_secs: u64,
}

/// Everything one running management surface owns.
struct Surface {
    identity: Client,
    backend: tokio::sync::Mutex<InternalAuthoringBackend>,
    org: Box<str>,
    project: Box<str>,
    tenant: Box<str>,
    login_ttl: Duration,
    session_ttl: Duration,
}

impl Surface {
    /// Reconcile a client-selected scope with the fixed scope this surface
    /// serves. A different project is refused exactly like a bad token.
    fn command_scope(&self, project_id: &str, environment: &str) -> Option<CommandScope> {
        if project_id != self.project.as_ref() || environment.is_empty() {
            return None;
        }
        Some(CommandScope {
            tenant_id: self.tenant.clone(),
            org: self.org.clone(),
            project: self.project.clone(),
            environment: environment.into(),
        })
    }
}

/// Serve the authenticated management authoring surface until the process ends.
pub async fn serve(args: ManagementServeArgs) -> anyhow::Result<()> {
    let address: SocketAddr = args
        .bind
        .parse()
        .with_context(|| format!("invalid management bind address {:?}", args.bind))?;
    let (identity, connection) = tokio_postgres::connect(&args.system_url, NoTls)
        .await
        .context("connect the T1 system database for identity")?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::error!(%error, "system identity database connection failed");
        }
    });
    let backend = InternalAuthoringBackend::connect(
        &args.authoring_database_url,
        args.tenant.clone(),
        args.source_schema.clone(),
    )
    .await?;
    let surface = Arc::new(Surface {
        identity,
        backend: tokio::sync::Mutex::new(backend),
        org: args.org.into_boxed_str(),
        project: args.project.into_boxed_str(),
        tenant: args.tenant.into_boxed_str(),
        login_ttl: Duration::from_secs(args.login_token_ttl_secs),
        session_ttl: Duration::from_secs(args.session_ttl_secs),
    });
    let listener = TcpListener::bind(address)
        .await
        .with_context(|| format!("bind management authoring surface on {address}"))?;
    tracing::info!(%address, "management authoring surface listening");
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("accept a management connection")?;
        let surface = Arc::clone(&surface);
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = service_fn(move |request| route(Arc::clone(&surface), request));
            if let Err(error) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await
            {
                tracing::debug!(%error, "management connection ended");
            }
        });
    }
}

async fn route(
    surface: Arc<Surface>,
    request: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let handled = match (request.method(), request.uri().path()) {
        (&Method::POST, "/login") => login(&surface, request).await,
        (&Method::POST, "/session") => open_session(&surface, request).await,
        (&Method::DELETE, "/session") => close_session(&surface, request).await,
        (&Method::POST, "/authoring") => authoring_command(&surface, request).await,
        _ => Ok(empty(StatusCode::NOT_FOUND)),
    };
    Ok(handled.unwrap_or_else(|error| {
        // The client learns nothing about an infrastructure fault.
        tracing::error!(%error, "management request failed");
        empty(StatusCode::INTERNAL_SERVER_ERROR)
    }))
}

/// Exchange a human's local secret for a personal access token.
///
/// Implements the wire contract frozen on `login_local`: `subject`, `secret`,
/// and `label` in; `token` and `expires_at` out. Every refusal — unknown
/// subject, wrong secret, disabled human, service principal, and malformed
/// input alike — answers with [`authorization_denied`].
async fn login(
    surface: &Surface,
    request: Request<Incoming>,
) -> anyhow::Result<Response<Full<Bytes>>> {
    let body = request.into_body().collect().await?.to_bytes();
    let Ok(login) = serde_json::from_slice::<LoginRequest>(&body) else {
        return Ok(empty(StatusCode::BAD_REQUEST));
    };
    match login_local(
        &surface.identity,
        &login.subject,
        login.secret.as_bytes(),
        &login.label,
        surface.login_ttl,
    )
    .await
    {
        Ok(Some(issued)) => Ok(json(
            StatusCode::OK,
            &LoginResponse {
                token: issued.token(),
                expires_at: issued.record().expires_at(),
            },
        )),
        Ok(None) => Ok(authorization_denied()),
        // Malformed input must not be distinguishable from a wrong secret.
        Err(error) if error.kind() == IdentityErrorKind::InvalidInput => Ok(authorization_denied()),
        Err(error) => Err(error.into()),
    }
}

/// Open a browser session for a human's local secret.
///
/// Implements the wire contract frozen on `login_session`: `subject` and
/// `secret` in; `csrf_token` and `expires_at` out, with the session value
/// itself in a `HttpOnly` `Set-Cookie` header the page cannot read. Every
/// refusal answers with [`authorization_denied`], exactly like `/login`.
///
/// This handler reads no cookie. The session it mints is always fresh, so a
/// value an attacker planted in the browser before login is not adopted, not
/// extended, and not returned — the caller leaves holding a new cookie for a new
/// row, which is what makes fixation fail.
async fn open_session(
    surface: &Surface,
    request: Request<Incoming>,
) -> anyhow::Result<Response<Full<Bytes>>> {
    let body = request.into_body().collect().await?.to_bytes();
    let Ok(login) = serde_json::from_slice::<SessionRequest>(&body) else {
        return Ok(empty(StatusCode::BAD_REQUEST));
    };
    match login_session(
        &surface.identity,
        &login.subject,
        login.secret.as_bytes(),
        surface.session_ttl,
    )
    .await
    {
        Ok(Some(issued)) => Ok(session_opened(&issued, surface.session_ttl)),
        Ok(None) => Ok(authorization_denied()),
        // Malformed input must not be distinguishable from a wrong secret.
        Err(error) if error.kind() == IdentityErrorKind::InvalidInput => Ok(authorization_denied()),
        Err(error) => Err(error.into()),
    }
}

/// Close the presented browser session.
///
/// Logout is a state change, so it carries the same CSRF proof every other state
/// change does — a cross-site page must not be able to log a user out. The proof
/// is checked before anything is revoked, and a token presenter is refused here
/// outright: this route belongs to the session presenter alone.
///
/// Revocation is a one-way stamp on the session row, so the cookie is dead for
/// every later request whether or not the browser honours the clearing header,
/// and a replayed logout is harmless.
async fn close_session(
    surface: &Surface,
    request: Request<Incoming>,
) -> anyhow::Result<Response<Full<Bytes>>> {
    let Some(Presenter::Session { cookie, csrf }) = presenter(&request) else {
        return Ok(authorization_denied());
    };
    if authenticate_session(&surface.identity, &cookie, &csrf)
        .await?
        .is_none()
    {
        return Ok(authorization_denied());
    }
    revoke_session(&surface.identity, &cookie).await?;
    Ok(session_closed())
}

/// Run one authoring command for a verified presenter.
///
/// Identity is settled from the credential headers alone, before the body is
/// read at all: no header and no request field can supply, override, or widen
/// the principal this command is attributed to. For a session presenter the CSRF
/// proof is settled in the same step, so a state change with no proof is refused
/// before the body is read, before route selection, and before the ledger is
/// touched — there is no path on which it mutates anything.
async fn authoring_command(
    surface: &Surface,
    request: Request<Incoming>,
) -> anyhow::Result<Response<Full<Bytes>>> {
    let Some(presented) = presenter(&request) else {
        return Ok(authorization_denied());
    };
    let resolved = match &presented {
        Presenter::Pat(token) => {
            authorize(&surface.identity, token, &surface.org, &surface.project).await?
        }
        Presenter::Session { cookie, csrf } => {
            authorize_session(
                &surface.identity,
                cookie,
                csrf,
                &surface.org,
                &surface.project,
            )
            .await?
        }
    };
    let Some(author) = resolved else {
        return Ok(authorization_denied());
    };

    let body = request.into_body().collect().await?.to_bytes();
    let Ok(text) = std::str::from_utf8(&body) else {
        return Ok(empty(StatusCode::BAD_REQUEST));
    };
    let document = match decode_document(text) {
        Ok(document) => document,
        Err(ContractDecodeError::UnsupportedContractVersion { requested }) => {
            return Ok(json(
                StatusCode::BAD_REQUEST,
                &AuthoringRefusal::UnsupportedContractVersion {
                    requested,
                    supported: SCHEMA_VERSION.to_owned(),
                },
            ));
        }
        // `deny_unknown_fields` on every contract type means a body that tries
        // to assert a principal lands here and never reaches a command.
        Err(_) => return Ok(empty(StatusCode::BAD_REQUEST)),
    };
    let AuthoringDocument::Request(command) = document else {
        return Ok(empty(StatusCode::BAD_REQUEST));
    };
    dispatch(surface, &author, &command).await
}

/// Dispatch one decoded command.
///
/// `save-flow-draft` is the command this transport mounts today; the rest of the
/// contract inventory answers `501` until the beads that own their handlers land
/// them. A `501` is the absence of a route, not a product refusal, so it carries
/// no document. Route selection happens here, after authorization, so naming an
/// unmounted kind is not a way to ask whether a route exists: an untrusted
/// presenter is refused identically whichever kind it names.
///
/// `wamn-ftfc.22` re-checked each remaining kind against this tree instead of
/// inheriting the reasons recorded when the route landed:
///
/// - `validate` has a backend, but three of its trusted inputs have no producer
///   here. [`crate::authoring::DraftBundleInputs`] needs execution-bundle and
///   plug identities nothing outside test fixtures constructs; the runner pin is
///   the digest of a compiled flowrunner this surface does not carry; and the
///   applied catalog identity is absent from the contract request. Supplying any
///   of them from a transport would persist a content-addressed pin that names
///   no real executable.
/// - `draft-run` has no backend: the only draft admission statement requires a
///   suite and a case, and one arbitrary input is neither.
/// - `suite-run` has a backend, but nothing resolves the contract's opaque
///   validated-draft handle back to the whole [`ValidatedDraftPin`] every read of
///   the validated-draft store requires, and this surface holds neither the
///   distinct runtime credential nor the compiled component execution needs.
/// - `publish` has no backend.
/// - `suite-projection` has its mapper in [`crate::projection`] and stays
///   unmounted by owner ruling while `wamn-rwcw` and `wamn-o6xw` are open: its
///   node, branch, edge, and refused-connection arrays are exhaustive
///   enumerations on the contract with no evidence source yet, so mounting it
///   would publish an empty enumeration as a complete one.
async fn dispatch(
    surface: &Surface,
    author: &AuthorizedAuthor,
    command: &AuthoringRequest,
) -> anyhow::Result<Response<Full<Bytes>>> {
    let AuthoringCommand::SaveFlowDraft(input) = &command.command else {
        return Ok(empty(StatusCode::NOT_IMPLEMENTED));
    };
    let Some(scope) = surface.command_scope(&input.scope.project_id, &input.scope.environment)
    else {
        return Ok(authorization_denied());
    };
    // The contract bounds `expected-revision` to the exactly representable wire
    // domain, so it always fits the `bigint` column behind it (wamn-ftfc.21).
    let expected_revision = i64::from(input.expected_revision);
    if input.draft_id.is_empty() || input.flow_id.is_empty() || command.command_id.is_empty() {
        return Ok(empty(StatusCode::BAD_REQUEST));
    }

    let request = SaveFlowDraft {
        tenant_id: scope.tenant_id.to_string(),
        draft_id: input.draft_id.clone(),
        flow_id: input.flow_id.clone(),
        expected_revision,
        definition: input.definition.clone(),
    };
    let mut backend = surface.backend.lock().await;
    let saved = save_flow_draft(
        &mut backend,
        author,
        &scope,
        &command.command_id,
        &request,
        input.provenance.as_ref(),
    )
    .await?;
    drop(backend);

    let outcome = match saved {
        SaveFlowDraftResult::Saved { revision, .. } => {
            let revision = SafeUint64::try_from(revision)
                .context("stored revision exceeds the exactly representable wire domain")?;
            AuthoringOutcome::Completed(Box::new(AuthoringSuccess::SaveFlowDraft(DraftIdentity {
                draft_id: input.draft_id.clone(),
                flow_id: input.flow_id.clone(),
                revision,
            })))
        }
        SaveFlowDraftResult::RevisionConflict => AuthoringOutcome::Refused(CommandRefusal {
            command: AuthoringCommandKind::SaveFlowDraft,
            reason: AuthoringRefusal::RevisionConflict {
                expected_revision: input.expected_revision,
                actual_revision: None,
            },
        }),
    };
    Ok(json(
        StatusCode::OK,
        &AuthoringDocument::Response(Box::new(AuthoringResponse {
            schema_version: SCHEMA_VERSION.to_owned(),
            command_id: command.command_id.clone(),
            outcome,
        })),
    ))
}

/// The credential one request presents. There is no third variant: a request
/// either carries a token, or carries a session cookie, or is anonymous.
enum Presenter {
    /// A headless caller's personal access token.
    Pat(String),
    /// A browser's session cookie and the CSRF proof it sent with it. `csrf` is
    /// empty when the header was absent, which the identity core refuses
    /// exactly as it refuses a wrong one — an absent proof is not a weaker
    /// failure than a forged one.
    Session { cookie: String, csrf: String },
}

/// Read the credential a request presents, consulting only credential headers.
///
/// A bearer token wins when both are present, so a page that somehow holds a
/// token is not silently downgraded to the cookie path. A cookie with no CSRF
/// header still produces a `Session`, deliberately: returning `None` there would
/// make an unproven state change indistinguishable from an anonymous one, and it
/// must be refused as the presenter it is.
fn presenter<B>(request: &Request<B>) -> Option<Presenter> {
    if let Some(token) = bearer(request) {
        return Some(Presenter::Pat(token.to_owned()));
    }
    let cookie = session_cookie(request)?;
    Some(Presenter::Session {
        cookie: cookie.to_owned(),
        csrf: csrf_proof(request).unwrap_or_default().to_owned(),
    })
}

/// Return the session cookie's value from the `cookie` header, ignoring every
/// other cookie the browser sent.
fn session_cookie<B>(request: &Request<B>) -> Option<&str> {
    request
        .headers()
        .get(hyper::header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            (name.trim() == SESSION_COOKIE_NAME).then(|| value.trim())
        })
        .filter(|value| !value.is_empty())
}

/// Return the presented CSRF proof, or `None` when the header is absent.
fn csrf_proof<B>(request: &Request<B>) -> Option<&str> {
    request.headers().get(CSRF_HEADER)?.to_str().ok()
}

/// Return the presented bearer token, or `None` when the header is absent or
/// does not carry exactly one non-empty `Bearer` credential.
fn bearer<B>(request: &Request<B>) -> Option<&str> {
    let value = request
        .headers()
        .get(hyper::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix(BEARER_SCHEME)?;
    (!value.is_empty()).then_some(value)
}

/// The single response every authentication and authorization failure returns.
///
/// Absent, malformed, forged, expired, revoked, disabled, unroled, and
/// cross-project presenters are byte-identical here, so a caller cannot use the
/// response to learn which predicate refused them. The body is the repository's
/// own frozen refusal literal; a pre-dispatch refusal carries no command
/// identity, so it is the bare reason rather than a response envelope.
fn authorization_denied() -> Response<Full<Bytes>> {
    json(
        StatusCode::FORBIDDEN,
        &AuthoringRefusal::AuthorizationDenied,
    )
}

/// Answer a successful login: the CSRF token in a body the page can read, the
/// session value in a cookie it cannot.
fn session_opened(issued: &IssuedSession, ttl: Duration) -> Response<Full<Bytes>> {
    let body = serde_json::to_vec(&SessionResponse {
        csrf_token: issued.csrf_token(),
        expires_at: issued.record().expires_at(),
    })
    .expect("contract documents serialize");
    Response::builder()
        .status(StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .header(
            hyper::header::SET_COOKIE,
            set_cookie(issued.cookie(), ttl.as_secs()),
        )
        .body(Full::new(Bytes::from(body)))
        .expect("static response builds")
}

/// Answer a successful logout. The clearing header is a courtesy to the browser;
/// the revocation stamp on the row is what actually ends the session.
fn session_closed() -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header(hyper::header::SET_COOKIE, set_cookie("", 0))
        .body(Full::new(Bytes::new()))
        .expect("static response builds")
}

/// Frame one session cookie with the frozen attribute set. `Max-Age` mirrors the
/// row's absolute expiry, so the browser and the database agree on the deadline.
fn set_cookie(value: &str, max_age_secs: u64) -> String {
    format!("{SESSION_COOKIE_NAME}={value}{SESSION_COOKIE_ATTRIBUTES}; Max-Age={max_age_secs}")
}

fn json(status: StatusCode, value: &impl Serialize) -> Response<Full<Bytes>> {
    let body = serde_json::to_vec(value).expect("contract documents serialize");
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .expect("static response builds")
}

fn empty(status: StatusCode) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::new()))
        .expect("static response builds")
}

/// The reserved login route's request document.
///
/// `deny_unknown_fields` is what keeps a client from smuggling an identity
/// assertion alongside its credential.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    subject: String,
    secret: String,
    label: String,
}

impl fmt::Debug for LoginRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginRequest")
            .field("subject", &self.subject)
            .field("secret", &"<redacted>")
            .field("label", &self.label)
            .finish()
    }
}

/// The reserved login route's response document. Field spellings are the frozen
/// wire contract on `login_local`, not this crate's naming convention.
#[derive(Clone, Serialize)]
struct LoginResponse<'a> {
    token: &'a str,
    expires_at: &'a str,
}

impl fmt::Debug for LoginResponse<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginResponse")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// The reserved session route's request document.
///
/// It carries no `label`: a browser session is not an operator-named credential
/// the way a token is. `deny_unknown_fields` is what keeps a client from
/// smuggling an identity assertion alongside its secret.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionRequest {
    subject: String,
    secret: String,
}

impl fmt::Debug for SessionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionRequest")
            .field("subject", &self.subject)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// The reserved session route's response document. Field spellings are the
/// frozen wire contract on `login_session`, not this crate's naming convention.
/// The session value is deliberately absent: it travels only in `Set-Cookie`.
#[derive(Clone, Serialize)]
struct SessionResponse<'a> {
    csrf_token: &'a str,
    expires_at: &'a str,
}

impl fmt::Debug for SessionResponse<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionResponse")
            .field("csrf_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The slice of this module between two top-level items, for the structural
    /// guards below.
    fn between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
        source
            .split(start)
            .nth(1)
            .unwrap_or_else(|| panic!("{start} exists"))
            .split(end)
            .next()
            .unwrap_or_else(|| panic!("{end} follows {start}"))
    }

    async fn body_of(response: Response<Full<Bytes>>) -> Bytes {
        response
            .into_body()
            .collect()
            .await
            .expect("a full body collects")
            .to_bytes()
    }

    #[tokio::test]
    async fn every_authentication_failure_shares_one_frozen_refusal_document() {
        assert_eq!(
            serde_json::to_string(&AuthoringRefusal::AuthorizationDenied).unwrap(),
            r#"{"kind":"authorization-denied"}"#
        );
        let denied = authorization_denied();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            denied
                .headers()
                .get(hyper::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        // Byte identity is the anti-oracle property: two refusals raised for
        // different reasons must be indistinguishable on the wire.
        let repeated = authorization_denied();
        assert_eq!(denied.status(), repeated.status());
        assert_eq!(denied.headers(), repeated.headers());
        assert_eq!(
            body_of(denied).await,
            Bytes::from_static(br#"{"kind":"authorization-denied"}"#)
        );
        assert_eq!(
            body_of(repeated).await,
            Bytes::from_static(br#"{"kind":"authorization-denied"}"#)
        );
    }

    #[test]
    fn only_an_exact_bearer_credential_is_read_as_a_token() {
        let build = |header: Option<&str>| {
            let mut request = Request::builder().method(Method::POST).uri("/authoring");
            if let Some(header) = header {
                request = request.header(hyper::header::AUTHORIZATION, header);
            }
            request.body(()).unwrap()
        };
        assert_eq!(
            bearer(&build(Some("Bearer wamn_pat_abc"))),
            Some("wamn_pat_abc")
        );
        for absent in [
            None,
            Some(""),
            Some("Bearer "),
            Some("bearer wamn_pat_abc"),
            Some("Basic wamn_pat_abc"),
            Some("wamn_pat_abc"),
        ] {
            assert!(bearer(&build(absent)).is_none(), "accepted {absent:?}");
        }
    }

    #[test]
    fn ledger_command_literals_match_the_wire_contract_spelling() {
        for (kind, audited) in [
            (
                AuthoringCommandKind::SaveFlowDraft,
                AuditedCommand::SaveFlowDraft,
            ),
            (AuthoringCommandKind::Validate, AuditedCommand::Validate),
            (AuthoringCommandKind::DraftRun, AuditedCommand::DraftRun),
            (AuthoringCommandKind::SuiteRun, AuditedCommand::SuiteRun),
            (AuthoringCommandKind::Publish, AuditedCommand::Publish),
            (
                AuthoringCommandKind::SuiteProjection,
                AuditedCommand::SuiteProjection,
            ),
        ] {
            let wire = serde_json::to_string(&kind).unwrap();
            assert_eq!(
                format!("\"{}\"", audited.as_str()),
                wire,
                "ledger literal drifted from the contract for {kind:?}"
            );
        }
        // The two host-side mutations have no client command and must not
        // collide with a contract literal.
        for host_side in [
            AuditedCommand::GrantDraftSafeGeneration,
            AuditedCommand::RevokeDraftSafeGeneration,
        ] {
            assert!(host_side.as_str().ends_with("-draft-safe-generation"));
        }
    }

    #[test]
    fn the_audit_statement_writes_every_attribution_column() {
        for column in [
            "tenant_id",
            "command_id",
            "command_kind",
            "principal_id",
            "principal_kind",
            "principal_subject",
            "effective_role",
            "org",
            "project",
            "environment",
            "target_ref",
            "provenance_commit",
            "provenance_ref",
            "provenance_dirty",
        ] {
            assert!(
                INSERT_COMMAND_AUDIT_SQL.contains(column),
                "audit insert drops {column}"
            );
        }
        assert!(INSERT_COMMAND_AUDIT_SQL.contains("catalog.authoring_command_audit"));
        assert!(INSERT_COMMAND_AUDIT_SQL.starts_with("INSERT INTO"));
        assert!(
            !INSERT_COMMAND_AUDIT_SQL
                .to_ascii_uppercase()
                .contains("UPDATE")
        );
    }

    #[test]
    fn credential_documents_never_reach_debug_output() {
        let login = LoginRequest {
            subject: "author@example.com".to_owned(),
            secret: "correct horse battery staple".to_owned(),
            label: "laptop".to_owned(),
        };
        let rendered = format!("{login:?}");
        assert!(!rendered.contains(&login.secret), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");

        let response = LoginResponse {
            token: "wamn_pat_0123456789abcdef_secret",
            expires_at: "2026-08-08T11:00:00Z",
        };
        let rendered = format!("{response:?}");
        assert!(!rendered.contains(response.token), "{rendered}");
        assert!(rendered.contains("2026-08-08T11:00:00Z"), "{rendered}");
    }

    #[test]
    fn login_documents_refuse_a_smuggled_identity_assertion() {
        let honest = r#"{"subject":"a@example.com","secret":"s","label":"laptop"}"#;
        let parsed = serde_json::from_str::<LoginRequest>(honest).expect("honest login decodes");
        assert_eq!(parsed.subject, "a@example.com");
        for injected in [
            r#"{"subject":"a@example.com","secret":"s","label":"l","principal":"root"}"#,
            r#"{"subject":"a@example.com","secret":"s","label":"l","principal-id":"root"}"#,
            r#"{"subject":"a@example.com","secret":"s","label":"l","role":"project-admin"}"#,
        ] {
            assert!(
                serde_json::from_str::<LoginRequest>(injected).is_err(),
                "accepted smuggled identity {injected}"
            );
        }
    }

    #[test]
    fn login_responses_carry_the_frozen_wire_field_names() {
        let rendered = serde_json::to_string(&LoginResponse {
            token: "wamn_pat_0123456789abcdef_secret",
            expires_at: "2026-08-08T11:00:00Z",
        })
        .unwrap();
        assert_eq!(
            rendered,
            r#"{"token":"wamn_pat_0123456789abcdef_secret","expires_at":"2026-08-08T11:00:00Z"}"#
        );
    }

    /// The cookie framing is the browser-side half of the defence, so every
    /// attribute is pinned byte for byte.
    #[tokio::test]
    async fn session_cookies_carry_the_frozen_attribute_set() {
        assert_eq!(
            set_cookie("wamn_sess_0123456789abcdef_secret", 43200),
            "wamn_session=wamn_sess_0123456789abcdef_secret; HttpOnly; SameSite=Strict; \
             Secure; Path=/; Max-Age=43200"
        );
        // Logout clears the value and expires it immediately, with the same
        // attributes — a browser only replaces a cookie when they all match.
        let closed = session_closed();
        assert_eq!(closed.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            closed
                .headers()
                .get(hyper::header::SET_COOKIE)
                .and_then(|value| value.to_str().ok()),
            Some("wamn_session=; HttpOnly; SameSite=Strict; Secure; Path=/; Max-Age=0")
        );
        assert!(
            body_of(closed).await.is_empty(),
            "logout carries a document"
        );

        // Each attribute earns its place; none may be dropped silently.
        for attribute in ["HttpOnly", "SameSite=Strict", "Secure", "Path=/"] {
            assert!(
                SESSION_COOKIE_ATTRIBUTES.contains(attribute),
                "the session cookie lost {attribute}"
            );
        }
        assert_eq!(CSRF_HEADER, "x-wamn-csrf");
        assert_eq!(SESSION_COOKIE_NAME, "wamn_session");
    }

    #[test]
    fn session_documents_are_frozen_and_never_carry_the_cookie() {
        let rendered = serde_json::to_string(&SessionResponse {
            csrf_token: "0123456789abcdef",
            expires_at: "2026-08-08T11:00:00Z",
        })
        .unwrap();
        assert_eq!(
            rendered,
            r#"{"csrf_token":"0123456789abcdef","expires_at":"2026-08-08T11:00:00Z"}"#
        );
        // The session value travels in `Set-Cookie` alone. A body field carrying
        // it would hand it straight back to the page script `HttpOnly` exists to
        // keep it away from.
        for leaked in ["cookie", "session", "wamn_sess_"] {
            assert!(!rendered.contains(leaked), "{leaked} reached the body");
        }

        let honest = r#"{"subject":"a@example.com","secret":"s"}"#;
        let parsed = serde_json::from_str::<SessionRequest>(honest).expect("honest login decodes");
        assert_eq!(parsed.subject, "a@example.com");
        for injected in [
            r#"{"subject":"a@example.com","secret":"s","principal":"root"}"#,
            r#"{"subject":"a@example.com","secret":"s","role":"project-admin"}"#,
            r#"{"subject":"a@example.com","secret":"s","csrf_token":"forged"}"#,
        ] {
            assert!(
                serde_json::from_str::<SessionRequest>(injected).is_err(),
                "accepted smuggled field {injected}"
            );
        }
    }

    #[test]
    fn session_credentials_never_reach_debug_output() {
        let request = SessionRequest {
            subject: "author@example.com".to_owned(),
            secret: "correct horse battery staple".to_owned(),
        };
        let rendered = format!("{request:?}");
        assert!(!rendered.contains(&request.secret), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");

        let response = SessionResponse {
            csrf_token: "0123456789abcdef",
            expires_at: "2026-08-08T11:00:00Z",
        };
        let rendered = format!("{response:?}");
        assert!(!rendered.contains(response.csrf_token), "{rendered}");
        assert!(rendered.contains("2026-08-08T11:00:00Z"), "{rendered}");
    }

    #[test]
    fn only_the_named_session_cookie_is_read_as_a_credential() {
        let build = |cookie: Option<&str>, csrf: Option<&str>| {
            let mut request = Request::builder().method(Method::POST).uri("/authoring");
            if let Some(cookie) = cookie {
                request = request.header(hyper::header::COOKIE, cookie);
            }
            if let Some(csrf) = csrf {
                request = request.header(CSRF_HEADER, csrf);
            }
            request.body(()).unwrap()
        };
        assert_eq!(
            session_cookie(&build(Some("wamn_session=abc"), None)),
            Some("abc")
        );
        // The browser sends every cookie for the path; only ours is a credential.
        assert_eq!(
            session_cookie(&build(Some("theme=dark; wamn_session=abc; other=1"), None)),
            Some("abc")
        );
        for absent in [None, Some(""), Some("wamn_session="), Some("other=abc")] {
            assert!(
                session_cookie(&build(absent, None)).is_none(),
                "accepted {absent:?}"
            );
        }

        // A cookie with no CSRF header is still a presenter, carrying an empty
        // proof — so it is refused as an unproven session, not as an anonymous
        // request. Those are different refusals to reason about, even though the
        // caller sees one document.
        let unproven = presenter(&build(Some("wamn_session=abc"), None));
        assert!(matches!(
            unproven,
            Some(Presenter::Session { ref csrf, .. }) if csrf.is_empty()
        ));
        let proven = presenter(&build(Some("wamn_session=abc"), Some("proof")));
        assert!(matches!(
            proven,
            Some(Presenter::Session { ref csrf, .. }) if csrf == "proof"
        ));
        assert!(presenter(&build(None, Some("proof"))).is_none());

        // A bearer token wins over a cookie, so a caller holding both is never
        // silently downgraded onto the path that needs a CSRF proof.
        let both = Request::builder()
            .method(Method::POST)
            .uri("/authoring")
            .header(hyper::header::AUTHORIZATION, "Bearer wamn_pat_abc")
            .header(hyper::header::COOKIE, "wamn_session=abc")
            .body(())
            .unwrap();
        assert!(matches!(presenter(&both), Some(Presenter::Pat(token)) if token == "wamn_pat_abc"));
    }

    /// Both presenters end at one role resolution, which is what makes the
    /// session a presenter rather than a second authority.
    #[test]
    fn both_presenters_resolve_through_one_role_seam() {
        let source = include_str!("management.rs");
        let region = between(
            source,
            "pub async fn authorize(",
            "/// Return the strongest admitted role",
        );
        assert!(region.contains("authenticate_pat(system_client, token)"));
        assert!(region.contains("authenticate_session(system_client, cookie, csrf_token)"));
        // Neither presenter reads roles for itself.
        assert_eq!(
            region.matches("project_roles(").count(),
            1,
            "a presenter resolved roles on its own instead of through role_for"
        );
        assert_eq!(region.matches("admitted_role(").count(), 1);
        assert_eq!(
            region.matches("role_for(system_client").count(),
            2,
            "a presenter bypassed the shared role seam"
        );
        // No JWT, no alternate role store, no second vocabulary of authority.
        let implementation = source
            .split("#[cfg(test)]")
            .next()
            .expect("the module has an implementation");
        for forbidden in ["jwt", "Jwt", "jsonwebtoken", "oidc", "Oidc", "openid"] {
            assert!(
                !implementation.contains(forbidden),
                "the session presenter reached for {forbidden}"
            );
        }
    }

    #[test]
    fn roles_order_admin_above_author_and_ignore_unknown_slugs() {
        assert!(ManagementRole::ProjectAdmin > ManagementRole::ProjectAuthor);
        assert_eq!(
            admitted_role(&[]),
            None,
            "a principal with no role must not be admitted"
        );
        for unknown in ["", "viewer", "Project-Admin", "admin", "project-deployer"] {
            assert!(
                ManagementRole::parse(unknown).is_none(),
                "admitted unknown role {unknown:?}"
            );
        }
        assert_eq!(
            ManagementRole::parse("project-author"),
            Some(ManagementRole::ProjectAuthor)
        );
        assert_eq!(
            ManagementRole::parse("project-admin"),
            Some(ManagementRole::ProjectAdmin)
        );
    }

    #[test]
    fn the_surface_serves_exactly_the_four_reserved_routes() {
        let source = include_str!("management.rs");
        let router = between(source, "async fn route(", "async fn login(");
        for route in [
            r#"(&Method::POST, "/login")"#,
            r#"(&Method::POST, "/session")"#,
            r#"(&Method::DELETE, "/session")"#,
            r#"(&Method::POST, "/authoring")"#,
        ] {
            assert!(router.contains(route), "missing reserved route {route}");
        }
        assert_eq!(
            router.matches("(&Method::").count(),
            4,
            "the reserved-route set changed without this guard moving"
        );
        assert!(router.contains("_ => Ok(empty(StatusCode::NOT_FOUND))"));
        // The token route keeps its own path and its own frozen response: the
        // browser presenter was added beside it, not over it.
        assert!(source.contains("struct LoginResponse"));
        assert!(source.contains("struct SessionResponse"));
    }

    #[test]
    fn identity_is_settled_before_the_request_body_is_read() {
        let source = include_str!("management.rs");
        let handler = between(source, "async fn authoring_command(", "/// Dispatch one");
        let authorized_at = handler.find("authorize(").expect("the handler authorizes");
        let body_at = handler
            .find("request.into_body()")
            .expect("the handler reads a body");
        assert!(
            authorized_at < body_at,
            "the request body is read before identity is settled"
        );
        // Settling identity is not the same as acting on it. The refusal has to
        // precede the body too, or the handler could decode a command, select a
        // route, and answer `501` for an unmounted kind before it ever refused
        // an untrusted presenter — which would make the absence of a route
        // readable without a credential. Measured from the check itself, because
        // the handler also refuses an absent credential earlier and that refusal
        // says nothing about this order.
        let refused_at = authorized_at
            + handler[authorized_at..]
                .find("return Ok(authorization_denied())")
                .expect("the handler refuses an unauthorized presenter");
        assert!(
            refused_at < body_at,
            "an unauthorized presenter reaches route selection before it is refused"
        );
        // Credential headers are the only headers this handler consults, and it
        // reads them through `presenter`, exactly once.
        assert_eq!(handler.matches(".headers()").count(), 0);
        assert_eq!(handler.matches("presenter(&request)").count(), 1);

        // For the session presenter the CSRF proof is part of settling identity,
        // not a later check: it is passed into authorization, which happens
        // before the body is read. A proof checked after this point could not
        // stop a command that had already decoded and dispatched.
        let csrf_at = handler
            .find("authorize_session(")
            .expect("the session branch authorizes");
        assert!(
            csrf_at < body_at,
            "a session request reaches its body before its CSRF proof is checked"
        );
        // Anchored on the call, not on its formatting: the argument list has to
        // carry the presented proof, or authorization would be deciding on the
        // cookie alone.
        let call = &handler[csrf_at..body_at];
        assert!(
            call.contains("csrf"),
            "the session branch authorized without passing its CSRF proof"
        );

        // `presenter` is the one place a credential is read, and it reads only
        // the three credential-bearing headers.
        let extraction = between(
            source,
            "fn presenter<B>(",
            "/// Return the presented bearer",
        );
        for allowed in [
            "bearer(request)",
            "session_cookie(request)",
            "csrf_proof(request)",
        ] {
            assert!(extraction.contains(allowed), "presenter dropped {allowed}");
        }
        let readers = between(source, "fn session_cookie<B>(", "fn bearer<B>(");
        assert_eq!(
            readers.matches(".headers()").count(),
            2,
            "a credential reader consults a header it does not own"
        );
        assert!(readers.contains("hyper::header::COOKIE"));
        assert!(readers.contains("CSRF_HEADER"));
    }

    /// Logout is a state change, so it is proven before it changes anything.
    #[test]
    fn logout_checks_its_csrf_proof_before_it_revokes() {
        let source = include_str!("management.rs");
        let handler = between(
            source,
            "async fn close_session(",
            "/// Run one authoring command",
        );
        let checked = handler
            .find("authenticate_session(&surface.identity")
            .expect("logout verifies the presented session");
        let revoked = handler
            .find("revoke_session(&surface.identity")
            .expect("logout revokes the session");
        assert!(
            checked < revoked,
            "logout revokes before it verifies the request"
        );
        // A token presenter cannot drive this route at all.
        assert!(handler.contains("Presenter::Session { cookie, csrf }"));
        assert_eq!(handler.matches(".headers()").count(), 0);
    }

    /// Login mints a fresh session and never adopts a presented one.
    #[test]
    fn opening_a_session_reads_no_cookie_and_reuses_nothing() {
        let source = include_str!("management.rs");
        let handler = between(source, "async fn open_session(", "/// Close the presented");
        for adopted in [
            "session_cookie",
            "presenter(",
            "COOKIE",
            "csrf_proof",
            "Presenter::",
        ] {
            assert!(
                !handler.contains(adopted),
                "the login handler consulted {adopted}, so a planted session could survive login"
            );
        }
        assert_eq!(handler.matches(".headers()").count(), 0);
        // The only session it can answer with is the one `login_session` minted.
        assert!(handler.contains("login_session("));
        assert!(handler.contains("session_opened(&issued"));
    }

    #[test]
    fn every_authored_mutation_records_before_it_runs() {
        let source = include_str!("management.rs");
        for (start, end, command) in [
            (
                "pub async fn save_flow_draft(",
                "/// Validate one exact",
                "AuditedCommand::SaveFlowDraft",
            ),
            (
                "pub async fn validate_flow_draft(",
                "/// Grant one draft-safe",
                "AuditedCommand::Validate",
            ),
            (
                "pub async fn grant_draft_safe_generation(",
                "/// Revoke one draft-safe",
                "AuditedCommand::GrantDraftSafeGeneration",
            ),
            (
                "pub async fn revoke_draft_safe_generation(",
                "/// Arguments for the",
                "AuditedCommand::RevokeDraftSafeGeneration",
            ),
        ] {
            let body = between(source, start, end);
            let record_at = body
                .find("record(")
                .unwrap_or_else(|| panic!("{start} records an audit row"));
            let run_at = body
                .find("backend.")
                .unwrap_or_else(|| panic!("{start} runs a command"));
            assert!(record_at < run_at, "{start} runs before it attributes");
            assert!(
                body.contains(command),
                "{start} attributes the wrong command"
            );
        }
    }

    /// Attribution a client attaches to a submission is never an input to the
    /// answer "may this caller run this command?".
    ///
    /// A checkout client legitimately knows the commit its working tree came
    /// from, and `wamn-ftfc.2` admits that as attribution. This pins the half
    /// that must never move: the derivation below reads a presented credential
    /// and the roles storage already holds for that principal, and nothing
    /// else. A commit, a tag, a signature, a submitted author, or a
    /// client-chosen role would have to appear here to change its answer.
    #[test]
    fn no_client_supplied_attribution_reaches_authorization() {
        let source = include_str!("management.rs");
        let authorize = between(
            source,
            "pub async fn authorize(",
            "/// Return the strongest admitted role",
        );
        assert!(authorize.contains("authenticate_pat(system_client, token)"));
        assert!(authorize.contains(
            "project_roles(system_client, authenticated.principal().id(), org, project)"
        ));
        assert!(authorize.contains("admitted_role(&roles)"));
        let selection = between(source, "fn admitted_role(", "fn author_from_authenticated");
        assert!(selection.contains("ManagementRole::parse"));
        // The transport is the other place a submitted attribution could reach
        // the role a command runs under: it holds the request while it holds
        // the author. Whatever it derived from the credential is what it must
        // dispatch with, unchanged.
        let handler = between(source, "async fn authoring_command(", "/// Dispatch one");
        assert!(
            handler.contains("dispatch(surface, &author, &command)"),
            "the transport dispatches something other than the author it derived"
        );
        assert!(
            !handler.contains("ManagementRole"),
            "the transport chose a role instead of using the one authorization derived"
        );
        for attribution in [
            "provenance",
            "commit",
            "committer",
            "signature",
            "signed",
            "definition",
        ] {
            for (region, name) in [
                (authorize, "authorization"),
                (selection, "role selection"),
                (handler, "the transport"),
            ] {
                assert!(
                    !region.contains(attribution),
                    "{name} consulted {attribution}"
                );
            }
        }
    }

    /// Attribution is written once, to the ledger, and read by nothing.
    ///
    /// The audit insert is the only statement that may name a provenance
    /// column, and the command the transport builds may not carry one: a save
    /// with attribution and a save without it must reach `SaveFlowDraft`
    /// identical, or the outcome could differ.
    #[test]
    fn provenance_reaches_the_ledger_and_no_other_statement() {
        let source = include_str!("management.rs");
        let implementation = source
            .split("#[cfg(test)]")
            .next()
            .expect("the module has an implementation");
        let statements: Vec<&str> = implementation
            .match_indices("INSERT INTO")
            .chain(implementation.match_indices("SELECT "))
            .chain(implementation.match_indices("UPDATE "))
            .map(|(at, _)| &implementation[at..])
            .collect();
        assert_eq!(statements.len(), 1, "the transport gained a new statement");
        assert!(statements[0].starts_with("INSERT INTO catalog.authoring_command_audit"));

        let dispatch = between(
            source,
            "async fn dispatch(",
            "/// Return the presented bearer",
        );
        let command = between(dispatch, "let request = SaveFlowDraft {", "};");
        assert!(
            !command.contains("provenance"),
            "attribution reached the command: {command}"
        );
        // It is passed beside the command, to the audited boundary only.
        assert!(dispatch.contains("input.provenance.as_ref()"));
    }

    /// The platform stores submitted content; it does not become a Git client,
    /// and it hands no caller a database or operator authority.
    #[test]
    fn the_surface_runs_no_git_and_exposes_no_platform_authority() {
        let source = include_str!("management.rs");
        // Scan the implementation only: this module's own tests name the
        // machinery they forbid, and a guard that matched its own vocabulary
        // would prove nothing.
        let implementation = source
            .split("#[cfg(test)]")
            .next()
            .expect("the module has an implementation");
        for machinery in [
            "git2",
            "gix",
            "libgit2",
            "Command::new",
            "std::process",
            "clone_repo",
            "rev-parse",
            "pre-receive",
        ] {
            assert!(
                !implementation.contains(machinery),
                "the management surface reached for {machinery}"
            );
        }
        let manifest = include_str!("../Cargo.toml");
        for dependency in ["git2", "gix", "libgit2", "wamn-ctl"] {
            assert!(
                !manifest.contains(dependency),
                "the worker took a {dependency} dependency"
            );
        }
        // Both database URLs are server configuration read from argv or the
        // environment, never anything a request supplies or a response returns.
        let args = between(
            source,
            "pub struct ManagementServeArgs",
            "/// Everything one running management surface owns",
        );
        assert!(args.contains("WAMN_SYSTEM_URL"));
        assert!(args.contains("WAMN_AUTHORING_PG_URL"));
        let dispatch = between(
            source,
            "async fn dispatch(",
            "/// Return the presented bearer",
        );
        for authority in ["url", "Url", "connect(", "superuser"] {
            assert!(
                !dispatch.contains(authority),
                "dispatch reached for {authority}"
            );
        }
    }
}
