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
//! Trusted context never comes from the request. [`AuthorizedAuthor`] has no
//! public constructor and implements no deserialization trait, exactly like the
//! [`AuthenticatedPrincipal`] it is derived from, and no handler here reads any
//! header other than `authorization` or any body field naming an identity.

use std::convert::Infallible;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context as _;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio_postgres::{Client, GenericClient, NoTls};

use wamn_authoring_model::{
    AuthoringCommand, AuthoringCommandKind, AuthoringDocument, AuthoringOutcome, AuthoringRefusal,
    AuthoringRequest, AuthoringResponse, AuthoringSuccess, CommandRefusal, CommitProvenance,
    ContractDecodeError, DraftIdentity, SCHEMA_VERSION, SafeUint64, decode_document,
};
use wamn_platform_identity::{
    AuthenticatedPrincipal, PrincipalKind, ProjectRole, authenticate_pat, project_roles,
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
/// `wamn_run.operator_run_actions` carries the same shape for the same
/// reason.
const INSERT_COMMAND_AUDIT_SQL: &str = "INSERT INTO catalog.authoring_command_audit \
    (tenant_id, command_id, command_kind, principal_id, principal_kind, \
     principal_subject, effective_role, org, project, environment, target_ref, \
     provenance_commit, provenance_ref, provenance_dirty) \
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)";

/// Bearer scheme this surface accepts, including its single trailing space.
const BEARER_SCHEME: &str = "Bearer ";

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
    Publish,
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
            Self::Publish => "publish",
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

/// Resolve the role an already-authenticated principal may exercise.
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
/// this audit's own append-only posture preserves authorized attempts.
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

    /// Project schema containing authoring and run state.
    #[arg(long, default_value = "wamn_run")]
    pub source_schema: String,
}

/// Everything one running management surface owns.
struct Surface {
    identity: Client,
    backend: tokio::sync::Mutex<InternalAuthoringBackend>,
    org: Box<str>,
    project: Box<str>,
    tenant: Box<str>,
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
        (&Method::POST, "/authoring") => authoring_command(&surface, request).await,
        _ => Ok(empty(StatusCode::NOT_FOUND)),
    };
    Ok(handled.unwrap_or_else(|error| {
        // The client learns nothing about an infrastructure fault.
        tracing::error!(%error, "management request failed");
        empty(StatusCode::INTERNAL_SERVER_ERROR)
    }))
}

/// Run one authoring command for a verified PAT.
///
/// Identity is settled from the authorization header alone, before the body is
/// read at all: no header and no request field can supply, override, or widen
/// the principal this command is attributed to.
async fn authoring_command(
    surface: &Surface,
    request: Request<Incoming>,
) -> anyhow::Result<Response<Full<Bytes>>> {
    let Some(token) = bearer(&request) else {
        return Ok(authorization_denied());
    };
    let Some(author) = authorize(&surface.identity, token, &surface.org, &surface.project).await?
    else {
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
/// - `validate` has a backend, but this surface does not carry the exact loaded
///   flowrunner bytes from which the host must derive the trusted runtime
///   revision, and the applied catalog identity is absent from the contract
///   request. Supplying either from a transport would persist a content-addressed
///   pin that names no real executable.
/// - `draft-run` has no backend for admitting one arbitrary authored input.
/// - `publish` has no backend.
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
/// cross-project PATs are byte-identical here, so a caller cannot use the
/// response to learn which predicate refused them. The body is the repository's
/// own frozen refusal literal; a pre-dispatch refusal carries no command
/// identity, so it is the bare reason rather than a response envelope.
fn authorization_denied() -> Response<Full<Bytes>> {
    json(
        StatusCode::FORBIDDEN,
        &AuthoringRefusal::AuthorizationDenied,
    )
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
            (AuthoringCommandKind::Publish, AuditedCommand::Publish),
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
    fn the_surface_serves_only_the_pat_authenticated_authoring_route() {
        let source = include_str!("management.rs");
        let router = between(source, "async fn route(", "/// Run one authoring command");
        assert!(router.contains(r#"(&Method::POST, "/authoring")"#));
        assert_eq!(
            router.matches("(&Method::").count(),
            1,
            "the reserved-route set changed without this guard moving"
        );
        assert!(router.contains("_ => Ok(empty(StatusCode::NOT_FOUND))"));
        let implementation = source.split("#[cfg(test)]").next().unwrap();
        for removed in [
            "\"/login\"",
            "\"/session\"",
            "Set-Cookie",
            "SESSION_COOKIE",
            "CSRF_HEADER",
            "LoginRequest",
            "SessionResponse",
        ] {
            assert!(!implementation.contains(removed), "retained {removed}");
        }
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
        // Authorization is the only header this handler consults, through the
        // bearer helper exactly once.
        assert_eq!(handler.matches(".headers()").count(), 0);
        assert_eq!(handler.matches("bearer(&request)").count(), 1);
        let reader = between(source, "fn bearer<B>(", "/// The single response");
        assert_eq!(reader.matches(".headers()").count(), 1);
        assert!(reader.contains("hyper::header::AUTHORIZATION"));
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
