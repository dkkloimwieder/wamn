//! Authenticated management transport for the canonical authoring commands.
//!
//! This is the boundary `authoring.rs` deferred until item 5 owned retained
//! client identity. It verifies a personal-access-token presenter against the T1
//! system database, derives trusted principal and project-role context, and runs
//! the internal adapter in the same transaction that appends the completed
//! outcome to the command ledger. The mutation and completed ledger row are
//! atomic: neither commits without the other. The adapter itself stays
//! principal-free; only the ledger retains the verified attribution.
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
use tracing::Instrument as _;

use wamn_authoring_model::{
    AuthoringCommand, AuthoringDocument, AuthoringOutcome, AuthoringQuery, AuthoringQueryRequest,
    AuthoringRequest, AuthoringRequestEnvelope, AuthoringResponse, AuthoringResponseEnvelope,
    AuthoringSuccess, CommandRefusal, CommitProvenance, ContractDecodeErrorKind, DraftIdentity,
    SCHEMA_VERSION, SafeUint64, SaveFlowDraftRefusal, decode_document,
};
use wamn_platform_identity::{
    AuthenticatedPrincipal, PrincipalKind, ProjectRole, authenticate_pat, project_roles,
};

use crate::authoring::{InternalAuthoringBackend, SaveFlowDraft, SaveFlowDraftResult};

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
     request_hash, outcome_bytes, provenance_commit, provenance_ref, provenance_dirty) \
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)";

/// Serialize one principal-scoped retry identity before reading or executing.
/// Hash collisions only over-serialize unrelated commands; the exact primary
/// key and request hash still decide replay versus reuse.
const LOCK_COMMAND_RETRY_SQL: &str = "SELECT pg_catalog.pg_advisory_xact_lock( \
    pg_catalog.hashtextextended( \
      pg_catalog.jsonb_build_array($1::text, $2::text, $3::text)::text, 0))";

const SELECT_COMMAND_RETRY_SQL: &str = "SELECT request_hash, outcome_bytes \
    FROM catalog.authoring_command_audit \
    WHERE tenant_id = $1 AND principal_id = $2 AND command_id = $3";

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditedCommand {
    SaveFlowDraft,
    Validate,
    DraftRun,
    TestSetRun,
    Publish,
}

impl AuditedCommand {
    /// Return the stable ledger literal. The contract kinds keep exactly the
    /// spelling the wire contract uses; a unit test pins them to `serde`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SaveFlowDraft => "save-flow-draft",
            Self::Validate => "validate",
            Self::DraftRun => "draft-run",
            Self::TestSetRun => "test-set-run",
            Self::Publish => "publish",
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

/// Attribution used to append one completed command outcome atomically with its
/// mutation.
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct StoredCommandOutcome {
    request_hash: String,
    outcome_bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetryDecision {
    Execute,
    Replay,
    Reuse,
}

fn classify_retry(existing: Option<&StoredCommandOutcome>, request_hash: &str) -> RetryDecision {
    match existing {
        None => RetryDecision::Execute,
        Some(existing) if existing.request_hash == request_hash => RetryDecision::Replay,
        Some(_) => RetryDecision::Reuse,
    }
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
    client: &(impl GenericClient + Sync),
    audit: &CommandAudit,
    request_hash: &str,
    outcome_bytes: &[u8],
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
                &request_hash,
                &outcome_bytes,
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
    command: &AuthoringRequest,
    request: &SaveFlowDraft,
) -> anyhow::Result<Vec<u8>> {
    let AuthoringCommand::SaveFlowDraft(input) = &command.command else {
        anyhow::bail!("save handler received a non-save command");
    };
    let request_hash = canonical_request_hash(command)?;
    let audit = CommandAudit {
        scope: scope.clone(),
        command_id: command.command_id.clone().into(),
        command: AuditedCommand::SaveFlowDraft,
        author: author.clone(),
        target_ref: request.draft_id.clone().into(),
        provenance: input.provenance.clone(),
    };

    let (authority, transaction) = backend.begin_command_transaction(audit.tenant_id()).await?;
    transaction
        .query_one(
            LOCK_COMMAND_RETRY_SQL,
            &[
                &audit.scope.tenant_id.as_ref(),
                &audit.author.principal_id.as_ref(),
                &audit.command_id.as_ref(),
            ],
        )
        .await
        .context("serialize authoring command retry identity")?;
    let existing = transaction
        .query_opt(
            SELECT_COMMAND_RETRY_SQL,
            &[
                &audit.scope.tenant_id.as_ref(),
                &audit.author.principal_id.as_ref(),
                &audit.command_id.as_ref(),
            ],
        )
        .await
        .context("read authoring command retry outcome")?
        .map(|row| StoredCommandOutcome {
            request_hash: row.get(0),
            outcome_bytes: row.get(1),
        });
    match classify_retry(existing.as_ref(), &request_hash) {
        RetryDecision::Replay => {
            let outcome = existing
                .expect("replay requires a stored outcome")
                .outcome_bytes;
            transaction
                .commit()
                .await
                .context("commit exact retry read")?;
            return Ok(outcome);
        }
        RetryDecision::Reuse => {
            let outcome = command_response_bytes(
                &command.command_id,
                AuthoringOutcome::Refused(CommandRefusal::SaveFlowDraft(
                    SaveFlowDraftRefusal::CommandIdReuse,
                )),
            )?;
            transaction
                .commit()
                .await
                .context("commit divergent retry read")?;
            return Ok(outcome);
        }
        RetryDecision::Execute => {}
    }

    let saved = crate::authoring::save_flow_draft(&authority, &transaction, request).await?;
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
        SaveFlowDraftResult::RevisionConflict => AuthoringOutcome::Refused(
            CommandRefusal::SaveFlowDraft(SaveFlowDraftRefusal::RevisionConflict {
                expected_revision: input.expected_revision,
                actual_revision: None,
            }),
        ),
    };
    let outcome_bytes = command_response_bytes(&command.command_id, outcome)?;
    insert_command_audit(&transaction, &audit, &request_hash, &outcome_bytes).await?;
    transaction
        .commit()
        .await
        .context("commit authoring command and exact retry outcome")?;
    Ok(outcome_bytes)
}

fn canonical_request_hash(command: &AuthoringRequest) -> anyhow::Result<String> {
    let value = serde_json::to_value(command).context("project closed command request to JSON")?;
    Ok(crate::store::sha256(&wamn_flow::canonical_json_bytes(
        &value,
    )))
}

fn command_response_bytes(command_id: &str, outcome: AuthoringOutcome) -> anyhow::Result<Vec<u8>> {
    serde_json::to_vec(&AuthoringDocument::Response(Box::new(
        AuthoringResponseEnvelope::Command(AuthoringResponse {
            schema_version: SCHEMA_VERSION.to_owned(),
            command_id: command_id.to_owned(),
            outcome,
        }),
    )))
    .context("serialize exact authoring outcome envelope")
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
    query_adapter: UnmountedAuthoringQueryAdapter,
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
        query_adapter: UnmountedAuthoringQueryAdapter,
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
        Err(error) if error.kind() == ContractDecodeErrorKind::UnsupportedContractVersion => {
            return Ok(json(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({
                    "kind": "unsupported-contract-version",
                    "requested": error.requested().unwrap_or_default(),
                    "supported": SCHEMA_VERSION,
                }),
            ));
        }
        // `deny_unknown_fields` on every contract type means a body that tries
        // to assert a principal lands here and never reaches a command.
        Err(_) => return Ok(empty(StatusCode::BAD_REQUEST)),
    };
    let AuthoringDocument::Request(request) = document else {
        return Ok(empty(StatusCode::BAD_REQUEST));
    };
    match request.as_ref() {
        AuthoringRequestEnvelope::Command(command) => {
            dispatch_command(surface, &author, command).await
        }
        AuthoringRequestEnvelope::Query(query) => {
            surface.query_adapter.dispatch_unmounted(query).await
        }
    }
}

/// Explicit .7.3 query seam. Production mounting and storage reads belong to
/// .7.4 and its operation owners, so this adapter can only emit correlation and
/// report that the query is unmounted.
#[derive(Clone, Copy, Debug, Default)]
struct UnmountedAuthoringQueryAdapter;

impl UnmountedAuthoringQueryAdapter {
    async fn dispatch_unmounted(
        self,
        request: &AuthoringQueryRequest,
    ) -> anyhow::Result<Response<Full<Bytes>>> {
        let span = tracing::info_span!(
            "authoring_query",
            query_id = %request.query_id,
            query = query_kind(&request.query),
        );
        async { Ok(empty(StatusCode::NOT_IMPLEMENTED)) }
            .instrument(span)
            .await
    }
}

const fn query_kind(query: &AuthoringQuery) -> &'static str {
    match query {
        AuthoringQuery::ReadDraft(_) => "read-draft",
        AuthoringQuery::GetRun(_) => "get-run",
        AuthoringQuery::GetReport(_) => "get-report",
    }
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
async fn dispatch_command(
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
    let outcome_bytes = save_flow_draft(&mut backend, author, &scope, command, &request).await?;
    drop(backend);
    Ok(json_bytes(StatusCode::OK, outcome_bytes))
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
        &serde_json::json!({"kind": "authorization-denied"}),
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

fn json_bytes(status: StatusCode, body: Vec<u8>) -> Response<Full<Bytes>> {
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
    use wamn_authoring_model::{AuthoringCommandKind, AuthoringScope, GetRun, QueryId};

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
            (AuthoringCommandKind::TestSetRun, AuditedCommand::TestSetRun),
            (AuthoringCommandKind::Publish, AuditedCommand::Publish),
        ] {
            let wire = serde_json::to_string(&kind).unwrap();
            assert_eq!(
                format!("\"{}\"", audited.as_str()),
                wire,
                "ledger literal drifted from the contract for {kind:?}"
            );
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
            "request_hash",
            "outcome_bytes",
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
        for statement in [LOCK_COMMAND_RETRY_SQL, SELECT_COMMAND_RETRY_SQL] {
            for identity_parameter in ["$1", "$2", "$3"] {
                assert!(
                    statement.contains(identity_parameter),
                    "retry statement drops {identity_parameter}: {statement}"
                );
            }
        }
        assert_eq!(SELECT_COMMAND_RETRY_SQL.matches('$').count(), 3);
        assert!(
            SELECT_COMMAND_RETRY_SQL
                .contains("WHERE tenant_id = $1 AND principal_id = $2 AND command_id = $3")
        );
        assert!(SELECT_COMMAND_RETRY_SQL.contains("request_hash, outcome_bytes"));
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

    #[tokio::test]
    async fn query_adapter_is_trace_only_and_unmounted() {
        let request = AuthoringQueryRequest {
            schema_version: SCHEMA_VERSION.to_owned(),
            query_id: QueryId::try_from("query-1".to_owned()).unwrap(),
            query: AuthoringQuery::GetRun(GetRun {
                scope: AuthoringScope {
                    project_id: "project-a".to_owned(),
                    environment: "dev".to_owned(),
                },
                run_id: "run-1".to_owned(),
            }),
        };
        let response = UnmountedAuthoringQueryAdapter
            .dispatch_unmounted(&request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        assert!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty()
        );

        let source = include_str!("management.rs");
        let adapter = between(
            source,
            "struct UnmountedAuthoringQueryAdapter;",
            "/// Dispatch one decoded command",
        );
        for required in ["authoring_query", "query_id", "query_kind"] {
            assert!(adapter.contains(required), "query trace omits {required}");
        }
        for forbidden in [
            "backend",
            "record(",
            "INSERT ",
            "UPDATE ",
            "DELETE ",
            ".execute(",
            ".query(",
        ] {
            assert!(
                !adapter.contains(forbidden),
                "unmounted query adapter gained {forbidden}"
            );
        }
        let implementation = source.split("#[cfg(test)]").next().unwrap();
        assert!(!implementation.contains("authoring_query_log"));
        assert!(!implementation.contains("authoring_query_audit"));
    }

    #[test]
    fn canonical_request_hash_ignores_object_order_and_detects_content_change() {
        let request = |input: serde_json::Value| {
            serde_json::from_value::<AuthoringRequest>(serde_json::json!({
                "schema-version": "0.1",
                "command-id": "retry-1",
                "command": {
                    "kind": "draft-run",
                    "input": {
                        "scope": {"project-id": "project-a", "environment": "dev"},
                        "validated-draft": {"validated-draft-id": "validated-1"},
                        "input": input
                    }
                }
            }))
            .unwrap()
        };
        let left = request(serde_json::json!({"z": 1, "a": {"y": 2, "x": 3}}));
        let reordered = request(serde_json::json!({"a": {"x": 3, "y": 2}, "z": 1}));
        let changed = request(serde_json::json!({"a": {"x": 4, "y": 2}, "z": 1}));
        assert_eq!(
            canonical_request_hash(&left).unwrap(),
            canonical_request_hash(&reordered).unwrap()
        );
        assert_ne!(
            canonical_request_hash(&left).unwrap(),
            canonical_request_hash(&changed).unwrap()
        );
    }

    #[test]
    fn retry_classifier_is_exact_hash_or_reuse() {
        let stored = StoredCommandOutcome {
            request_hash: "sha256:exact".to_owned(),
            outcome_bytes: br#"{"document":"response"}"#.to_vec(),
        };
        assert_eq!(classify_retry(None, "sha256:exact"), RetryDecision::Execute);
        assert_eq!(
            classify_retry(Some(&stored), "sha256:exact"),
            RetryDecision::Replay
        );
        assert_eq!(
            classify_retry(Some(&stored), "sha256:changed"),
            RetryDecision::Reuse
        );
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
    fn every_authored_mutation_and_completed_outcome_commit_atomically() {
        let source = include_str!("management.rs");
        let body = between(
            source,
            "pub async fn save_flow_draft(",
            "fn canonical_request_hash(",
        );
        let lock_at = body.find("LOCK_COMMAND_RETRY_SQL").unwrap();
        let lookup_at = body.find("SELECT_COMMAND_RETRY_SQL").unwrap();
        let run_at = body
            .find("crate::authoring::save_flow_draft")
            .expect("the new identity executes the command");
        let record_at = body
            .find("insert_command_audit")
            .expect("the completed command stores its outcome");
        let commit_at = body.rfind(".commit()").expect("the transaction commits");
        assert!(lock_at < lookup_at && lookup_at < run_at);
        assert!(run_at < record_at && record_at < commit_at);
        assert!(body.contains("AuditedCommand::SaveFlowDraft"));

        let dispatch = between(
            source,
            "async fn dispatch_command(",
            "/// Return the presented",
        );
        assert_eq!(
            dispatch.matches("save_flow_draft(&mut backend").count(),
            1,
            "save is the sole mounted audited command"
        );
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
            handler.contains("dispatch_command(surface, &author, command)"),
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
        assert_eq!(implementation.matches("INSERT INTO").count(), 1);
        assert_eq!(implementation.matches("UPDATE ").count(), 0);
        assert!(INSERT_COMMAND_AUDIT_SQL.contains("provenance_commit"));
        assert!(INSERT_COMMAND_AUDIT_SQL.contains("provenance_ref"));
        assert!(INSERT_COMMAND_AUDIT_SQL.contains("provenance_dirty"));
        assert!(!LOCK_COMMAND_RETRY_SQL.contains("provenance"));
        assert!(!SELECT_COMMAND_RETRY_SQL.contains("provenance"));

        let dispatch = between(
            source,
            "async fn dispatch_command(",
            "/// Return the presented bearer",
        );
        let command = between(dispatch, "let request = SaveFlowDraft {", "};");
        assert!(
            !command.contains("provenance"),
            "attribution reached the command: {command}"
        );
        let handler = between(
            source,
            "pub async fn save_flow_draft(",
            "fn canonical_request_hash(",
        );
        assert!(handler.contains("provenance: input.provenance.clone()"));
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
            "async fn dispatch_command(",
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
