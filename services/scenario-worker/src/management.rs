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
    AuthoringCommand, AuthoringDocument, AuthoringOutcome, AuthoringQuery, AuthoringQueryOutcome,
    AuthoringQueryRequest, AuthoringQueryResponse, AuthoringQuerySuccess, AuthoringRequest,
    AuthoringRequestEnvelope, AuthoringResponse, AuthoringResponseEnvelope, AuthoringSuccess,
    CommandRefusal, CommitProvenance, ContractDecodeErrorKind, GateReceipt, GateRefusal,
    GetReportRefusal, QueryRefusal, ReportProjection, SCHEMA_VERSION, ValidatedDraftRef,
    decode_document,
};
use wamn_platform_identity::{
    AuthenticatedPrincipal, PrincipalKind, ProjectRole, authenticate_pat, project_roles,
};

use wamn_control_provision::{
    SystemReader, parse_control_authoring_url, parse_management_admission_url,
    parse_system_reader_url,
};

use crate::authoring::{ControlAuthoringScope, GetReportResult, InternalAuthoringBackend};

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

/// Append one accepted gate's durable report (wamn-0h0g.8.5.6).
///
/// Keyed by `wiring_hash` alone: a gate is effect-free, so the same document
/// always yields the same verdict and the report needs no minted identity. That
/// same reproducibility is why re-gating converges instead of conflicting —
/// `ON CONFLICT DO NOTHING` keeps the FIRST row, which is byte-identical to the
/// one this pass would have written. An `UPDATE` would be refused by the
/// relation's immutability trigger, and would be wrong if it were not.
///
/// Params: `$1` tenant, `$2` wiring hash, `$3` passed, `$4` summary.
const INSERT_GATE_REPORT_SQL: &str = "INSERT INTO wamn_run.gate_reports \
    (tenant_id, wiring_hash, passed, summary) VALUES ($1, $2, $3, $4) \
    ON CONFLICT (tenant_id, wiring_hash) DO NOTHING";

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
    Gate,
    Publish,
}

impl AuditedCommand {
    /// Return the stable ledger literal. The contract kinds keep exactly the
    /// spelling the wire contract uses; a unit test pins them to `serde`.
    pub const fn as_str(self) -> &'static str {
        match self {
            // `gate` is spelled `test-set-run` on the wire and in the ledger
            // until the wiring vocabulary sweep (wamn-0h0g.26.18).
            Self::Gate => "test-set-run",
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

/// Append one accepted gate's report on an already-scoped author credential.
///
/// Takes the whole [`GateReport`](crate::store::admission::GateReport) rather
/// than its parts, so a caller cannot pair one judgment's verdict with another
/// judgment's hash.
pub(crate) async fn insert_gate_report(
    client: &(impl GenericClient + Sync),
    tenant_id: &str,
    report: &crate::store::admission::GateReport,
) -> anyhow::Result<()> {
    client
        .execute(
            INSERT_GATE_REPORT_SQL,
            &[
                &tenant_id,
                &report.wiring_hash,
                &report.passed,
                &report.summary,
            ],
        )
        .await
        .context("append the accepted gate's report")?;
    Ok(())
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

fn canonical_request_hash(command: &AuthoringRequest) -> anyhow::Result<String> {
    let value = serde_json::to_value(command).context("project closed command request to JSON")?;
    Ok(crate::store::sha256(
        &wamn_execution_contract::canonical_json_bytes(&value),
    ))
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

fn query_response_bytes(
    query_id: &wamn_authoring_model::QueryId,
    outcome: AuthoringQueryOutcome,
) -> anyhow::Result<Vec<u8>> {
    serde_json::to_vec(&AuthoringDocument::Response(Box::new(
        AuthoringResponseEnvelope::Query(AuthoringQueryResponse {
            schema_version: SCHEMA_VERSION.to_owned(),
            query_id: query_id.clone(),
            outcome,
        }),
    )))
    .context("serialize exact authoring query outcome envelope")
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
    ///
    /// A SEPARATE identity-read connection (wamn-0h0g.8.18): it is never the
    /// authoring or report store, and the authoring credential is never used to
    /// read identity.
    ///
    /// A scoped A/B LOGIN generation of `wamn_identity_reader`, whose whole
    /// authority is SELECT on `identity.pats`, `identity.principals` and
    /// `identity.project_roles` (wamn-0h0g.12.67). It used to authenticate as
    /// `wamn_system`, which OWNS those relations under no row-level security —
    /// so whoever read that Secret could INSERT a token_hash/token_prefix pair
    /// bound to any principal and present the matching PAT here, or INSERT a
    /// project_roles row and self-grant in any project. It is settled purely
    /// before any I/O, so such a credential crash-loops instead of serving.
    #[arg(long = "system-url", env = "WAMN_SYSTEM_URL")]
    pub system_url: String,

    /// The SOLE authoring and report connection input: a scoped A/B generation of
    /// `wamn_control_author` on the CONTROL database (wamn-0h0g.8.18).
    ///
    /// There is no project-URL fallback, no dual read, and no dual write. An
    /// absent or out-of-scope value refuses before any I/O.
    #[arg(
        long = "control-authoring-database-url",
        env = "WAMN_CONTROL_AUTHORING_PG_URL"
    )]
    pub control_authoring_database_url: String,

    /// The project-environment admission connection input: a scoped A/B
    /// generation of `wamn_management_admitter` on THIS environment's PROJECT
    /// database (wamn-0h0g.8.5.3).
    ///
    /// It is a SECOND, separate connection, never a fallback for the authoring
    /// one and never reachable from it: admission writes project run state,
    /// authoring writes the control ledger, and a transaction cannot span two
    /// databases. Flipping the production admission path off the shared
    /// `wamn_app` role and onto this credential is wamn-0h0g.22.10's traffic
    /// change; this argument is the plumbing that change needs to already exist.
    ///
    /// **Deliberately has no `default_value`** — the shape wamn-0h0g.12.129 and
    /// .12.134 settled for every credential input. A default here would name a
    /// connection nobody chose: the value is a full postgres URL carrying a
    /// password, so a placeholder either points at a database this scope holds
    /// no credential for, or silently ships a publicly known one. The process
    /// refuses at parse time instead, and `value_name` puts the environment
    /// variable inside clap's own missing-argument error so an operator reading
    /// a crash-looping pod's logs is told which Secret key is absent.
    #[arg(
        long = "management-admission-database-url",
        env = "WAMN_MANAGEMENT_ADMISSION_PG_URL",
        value_name = "URL ($WAMN_MANAGEMENT_ADMISSION_PG_URL)"
    )]
    pub management_admission_database_url: String,

    /// Organization whose project roles admit a caller.
    #[arg(long, env = "WAMN_MANAGEMENT_ORG")]
    pub org: String,

    /// The single project this surface serves.
    #[arg(long, env = "WAMN_MANAGEMENT_PROJECT")]
    pub project: String,

    /// The single environment this surface serves.
    ///
    /// One management instance serves exactly one `(org, project, environment)`,
    /// so this is fixed configuration rather than a per-request choice, and it is
    /// half of what names the control-author generation this process may use.
    #[arg(long, env = "WAMN_MANAGEMENT_ENVIRONMENT")]
    pub environment: String,

    /// Fixed authoring tenant the adapter is scoped to.
    #[arg(long, env = "WAMN_MANAGEMENT_TENANT")]
    pub tenant: String,

    /// Control-store schema containing the reservation, case-map, and report
    /// relations. Unchanged by the residency move.
    #[arg(long, default_value = "wamn_run")]
    pub source_schema: String,
}

impl ManagementServeArgs {
    /// The single management scope this process is bound to.
    fn control_authoring_scope(&self) -> ControlAuthoringScope {
        ControlAuthoringScope {
            org: self.org.clone(),
            project: self.project.clone(),
            environment: self.environment.clone(),
            tenant_id: self.tenant.clone(),
            source_schema: self.source_schema.clone(),
        }
    }
}

/// Everything one running management surface owns.
struct Surface {
    identity: Client,
    backend: tokio::sync::Mutex<InternalAuthoringBackend>,
    /// The SECOND, separate connection: the project database this environment's
    /// runs live in (wamn-0h0g.8.5.3, consumed by wamn-0h0g.8.5.4). It is never
    /// the authoring one's fallback and never reachable from it.
    admission: tokio::sync::Mutex<crate::store::admission::AdmissionSurface>,
    query_adapter: AuthoringQueryAdapter,
    org: Box<str>,
    project: Box<str>,
    environment: Box<str>,
    tenant: Box<str>,
}

impl Surface {
    /// Reconcile a client-selected scope with the fixed scope this surface
    /// serves. A different project or environment is refused exactly like a bad
    /// token.
    ///
    /// The environment is pinned rather than accepted (wamn-0h0g.8.18): one
    /// management instance serves exactly one `(org, project, environment)`, and
    /// its control-author generation is named for that triple, so admitting
    /// another environment would attribute a command to a scope this process
    /// holds no credential for.
    fn command_scope(&self, project_id: &str, environment: &str) -> Option<CommandScope> {
        reconcile_command_scope(
            &self.tenant,
            &self.org,
            &self.project,
            &self.environment,
            project_id,
            environment,
        )
    }
}

/// The scope reconciliation, free of the live connections a [`Surface`] owns so
/// it can be exercised without one.
fn reconcile_command_scope(
    tenant: &str,
    org: &str,
    project: &str,
    environment: &str,
    requested_project: &str,
    requested_environment: &str,
) -> Option<CommandScope> {
    if requested_project != project || requested_environment != environment {
        return None;
    }
    Some(CommandScope {
        tenant_id: tenant.into(),
        org: org.into(),
        project: project.into(),
        environment: environment.into(),
    })
}

/// Serve the authenticated management authoring surface until the process ends.
///
/// ALL THREE connection inputs are settled FIRST and PURELY: an absent or
/// out-of-scope `WAMN_CONTROL_AUTHORING_PG_URL`, `WAMN_MANAGEMENT_ADMISSION_PG_URL`
/// or `WAMN_SYSTEM_URL` refuses before this function opens a file or a socket
/// (wamn-0h0g.8.18, wamn-0h0g.8.5.3, wamn-0h0g.12.67). None is another's
/// fallback: the first names the control database, the second names this
/// environment's project database, and the third reads identity out of the T1
/// system database.
pub async fn serve(args: ManagementServeArgs) -> anyhow::Result<()> {
    let scope = args.control_authoring_scope();
    // The accepted value is deliberately discarded: this call exists to establish
    // the ORDER, and the backend re-derives it so an in-process caller that never
    // goes through `serve` is held to the same gate.
    parse_control_authoring_url(
        &args.control_authoring_database_url,
        &scope.org,
        &scope.project,
        &scope.environment,
    )?;
    // The admission credential is proven in scope on the same terms, at the same
    // point, for the same reason. Its consumer is the sequential composition
    // (wamn-0h0g.8.5.4); refusing here means a mis-scoped Secret crash-loops at
    // startup instead of surfacing on the first admitted run. The connection
    // this value opens is established below, after both inputs have been
    // settled purely — the order, not the connection, is what this call fixes.
    parse_management_admission_url(
        &args.management_admission_database_url,
        &scope.org,
        &scope.project,
        &scope.environment,
    )?;
    // The identity READ credential is settled on the same terms, at the same
    // point, for the same reason (wamn-0h0g.12.67). It is the narrowest of the
    // three and the most dangerous to get wrong: this surface's whole
    // authorization model is rows in `identity.*`, which carries no row-level
    // security, so a connection input that authenticates as the schema owner
    // would let its own reader forge the answers it then trusts.
    parse_system_reader_url(
        SystemReader::Identity,
        &args.system_url,
        &scope.org,
        &scope.project,
        &scope.environment,
    )?;
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
    let backend =
        InternalAuthoringBackend::connect(&args.control_authoring_database_url, &scope).await?;
    let admission = crate::store::admission::AdmissionSurface::connect(
        &args.management_admission_database_url,
        &scope.org,
        &scope.project,
        &scope.environment,
        &scope.tenant_id,
    )
    .await?;
    let surface = Arc::new(Surface {
        identity,
        backend: tokio::sync::Mutex::new(backend),
        admission: tokio::sync::Mutex::new(admission),
        query_adapter: AuthoringQueryAdapter,
        org: args.org.into_boxed_str(),
        project: args.project.into_boxed_str(),
        environment: args.environment.into_boxed_str(),
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
            surface.query_adapter.dispatch(surface, query).await
        }
    }
}

/// Non-ledgered query adapter.
///
/// `get-report` reads only the fixed control-store scope, and it is now the
/// whole query inventory: `read-draft` collapsed with the draft concept
/// (wamn-0h0g.8.5.5).
#[derive(Clone, Copy, Debug, Default)]
struct AuthoringQueryAdapter;

impl AuthoringQueryAdapter {
    async fn dispatch(
        self,
        surface: &Surface,
        request: &AuthoringQueryRequest,
    ) -> anyhow::Result<Response<Full<Bytes>>> {
        let span = tracing::info_span!(
            "authoring_query",
            query_id = %request.query_id,
            query = query_kind(&request.query),
        );
        async {
            match &request.query {
                AuthoringQuery::GetReport(input) => get_report(surface, request, input).await,
            }
        }
        .instrument(span)
        .await
    }
}

async fn get_report(
    surface: &Surface,
    request: &AuthoringQueryRequest,
    input: &wamn_authoring_model::GetReport,
) -> anyhow::Result<Response<Full<Bytes>>> {
    let Some(scope) = surface.command_scope(&input.scope.project_id, &input.scope.environment)
    else {
        return Ok(authorization_denied());
    };
    if input.report_id.is_empty() {
        return Ok(empty(StatusCode::BAD_REQUEST));
    }

    let backend = surface.backend.lock().await;
    let result = backend
        .get_report(&scope.tenant_id, &input.report_id)
        .await?;
    drop(backend);
    let outcome = match result {
        GetReportResult::NotFound => AuthoringQueryOutcome::Refused(QueryRefusal::GetReport(
            GetReportRefusal::ReportNotFound {
                report_id: input.report_id.clone(),
            },
        )),
        GetReportResult::Finalized {
            validated_draft_id,
            passed,
            summary,
        } => AuthoringQueryOutcome::Completed(Box::new(AuthoringQuerySuccess::GetReport(
            ReportProjection::Finalized {
                report_id: input.report_id.clone(),
                validated_draft: ValidatedDraftRef { validated_draft_id },
                passed,
                summary,
            },
        ))),
    };
    let body = query_response_bytes(&request.query_id, outcome)?;
    Ok(json_bytes(StatusCode::OK, body))
}

const fn query_kind(query: &AuthoringQuery) -> &'static str {
    match query {
        AuthoringQuery::GetReport(_) => "get-report",
    }
}

/// Dispatch one decoded command.
///
/// `gate` is the one command this transport mounts; `publish` answers `501`
/// until the bead that owns its handler lands it. A `501` is the absence of a
/// route, not a product refusal, so it carries no document. Route selection
/// happens here, after authorization, so naming an unmounted kind is not a way
/// to ask whether a route exists: an untrusted presenter is refused identically
/// whichever kind it names.
///
/// The three kinds that used to answer `501` here — `save-draft`, `validate` and
/// `draft-run` — are gone from the contract entirely (wamn-0h0g.8.5.5) rather
/// than still unmounted.
async fn dispatch_command(
    surface: &Surface,
    author: &AuthorizedAuthor,
    command: &AuthoringRequest,
) -> anyhow::Result<Response<Full<Bytes>>> {
    match command_route(&command.command) {
        CommandRoute::Gate(input) => gate_route(surface, author, command, input).await,
        CommandRoute::Unmounted => Ok(route_absent()),
    }
}

/// Which handler, if any, one decoded command selects.
///
/// Route selection is a PURE function of the command, split out from
/// [`dispatch_command`] so the mounted inventory can be decided by CALLING it
/// rather than by reading the dispatcher's text. `dispatch_command` has no
/// second opinion: it matches on this answer and nothing else, so the two cannot
/// drift (wamn-0h0g.8.5.4 — the source scan this replaced asserted a substring
/// count over its own source, which the owner ruled the weakest form of proof).
#[derive(Debug)]
enum CommandRoute<'a> {
    Gate(&'a wamn_authoring_model::Gate),
    Unmounted,
}

const fn command_route(command: &AuthoringCommand) -> CommandRoute<'_> {
    match command {
        AuthoringCommand::Gate(input) => CommandRoute::Gate(input),
        AuthoringCommand::Publish(_) => CommandRoute::Unmounted,
    }
}

/// The one answer an unmounted kind receives.
///
/// A `501` is the absence of a route, not a product refusal, so it carries no
/// document: nothing here composes an outcome envelope, and there is no branch
/// that could put one in.
fn route_absent() -> Response<Full<Bytes>> {
    empty(StatusCode::NOT_IMPLEMENTED)
}

async fn gate_route(
    surface: &Surface,
    author: &AuthorizedAuthor,
    command: &AuthoringRequest,
    input: &wamn_authoring_model::Gate,
) -> anyhow::Result<Response<Full<Bytes>>> {
    let Some(scope) = surface.command_scope(&input.scope.project_id, &input.scope.environment)
    else {
        return Ok(authorization_denied());
    };
    if input.validated_draft.validated_draft_id.is_empty() || command.command_id.is_empty() {
        return Ok(empty(StatusCode::BAD_REQUEST));
    }
    let mut backend = surface.backend.lock().await;
    let admission = surface.admission.lock().await;
    let outcome_bytes = gate(&mut backend, &admission, author, &scope, command, input).await?;
    drop(admission);
    drop(backend);
    Ok(json_bytes(StatusCode::OK, outcome_bytes))
}

/// Judge one candidate against its own `cases` array, attributing the judgment
/// to its verified author.
///
/// # The judgment reads; this transaction writes both of its facts at once
///
/// wamn-0h0g.8.5.5: a gate is a judgment about a document, not an execution of
/// it, so [`run_gate`](crate::store::admission::run_gate) reads the PROJECT
/// database and mutates nothing there. Its two durable consequences are written
/// here, in the CONTROL database, in ONE transaction: the attribution ledger
/// row, and — for an ACCEPTED judgment only — the report row keyed by the
/// candidate's wiring hash (wamn-0h0g.8.5.6). A refusal is not a report, so a
/// refused command writes only the ledger row.
///
/// Both are written LAST, after an outcome exists. Splitting them would leave
/// either an attributed judgment whose report no query can resolve — precisely
/// the hole this bead closes — or a report nothing accounts for.
///
/// An exact retry therefore converges trivially: a pass interrupted before the
/// commit leaves neither row, so the retry classifies as `Execute` and
/// re-derives the same judgment from the same immutable candidate. Once the
/// ledger row exists the command is finished, and the retry replays the stored
/// receipt.
async fn gate(
    backend: &mut InternalAuthoringBackend,
    admission: &crate::store::admission::AdmissionSurface,
    author: &AuthorizedAuthor,
    scope: &CommandScope,
    command: &AuthoringRequest,
    input: &wamn_authoring_model::Gate,
) -> anyhow::Result<Vec<u8>> {
    let request_hash = canonical_request_hash(command)?;
    let audit = CommandAudit {
        scope: scope.clone(),
        command_id: command.command_id.clone().into(),
        command: AuditedCommand::Gate,
        author: author.clone(),
        target_ref: input.validated_draft.validated_draft_id.clone().into(),
        // The gate command carries no source claim, and nothing on this path
        // reads one. The ledger's provenance columns stay writable for a command
        // that does; every surviving command writes them NULL.
        provenance: None,
    };
    if let Some(settled) = settle_retry(backend, &audit, &request_hash).await? {
        return Ok(settled);
    }

    let judgment = crate::store::admission::run_gate(
        admission,
        &crate::store::admission::GateRequest {
            environment: &scope.environment,
            validated_draft_id: &input.validated_draft.validated_draft_id,
        },
    )
    .await?;
    // An accepted judgment carries the report this command must persist; a
    // refusal carries none, which is what makes "no row" and "report-not-found"
    // the same fact rather than two.
    let (outcome, report) = match judgment {
        crate::store::admission::GateJudgment::Accepted(report) => (
            AuthoringOutcome::Completed(Box::new(AuthoringSuccess::Gate(GateReceipt {
                // The receipt hands back the key the report is stored under, so
                // `get-report` resolves exactly what the gate wrote.
                report_id: report.wiring_hash.clone(),
                validated_draft: input.validated_draft.clone(),
            }))),
            Some(report),
        ),
        crate::store::admission::GateJudgment::Refused(refusal) => (
            AuthoringOutcome::Refused(CommandRefusal::Gate(refusal)),
            None,
        ),
    };
    let outcome_bytes = command_response_bytes(&command.command_id, outcome)?;

    let (_, transaction) = backend.begin_command_transaction(audit.tenant_id()).await?;
    lock_retry_identity(&transaction, &audit).await?;
    // Re-read under the lock: a concurrent pass may have finished the identical
    // composition while this one ran, and its stored outcome is the one answer.
    if let Some(existing) = read_retry_outcome(&transaction, &audit).await? {
        let settled = match classify_retry(Some(&existing), &request_hash) {
            RetryDecision::Replay => existing.outcome_bytes,
            _ => gate_command_id_reuse(&command.command_id)?,
        };
        transaction.commit().await.context("commit retry read")?;
        return Ok(settled);
    }
    if let Some(report) = &report {
        insert_gate_report(&transaction, audit.tenant_id(), report).await?;
    }
    insert_command_audit(&transaction, &audit, &request_hash, &outcome_bytes).await?;
    transaction
        .commit()
        .await
        .context("commit the gate command outcome")?;
    Ok(outcome_bytes)
}

/// Serialize this principal-scoped retry identity inside a command transaction.
async fn lock_retry_identity(
    transaction: &(impl GenericClient + Sync),
    audit: &CommandAudit,
) -> anyhow::Result<()> {
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
    Ok(())
}

/// Read this retry identity's stored outcome, if one was already recorded.
async fn read_retry_outcome(
    transaction: &(impl GenericClient + Sync),
    audit: &CommandAudit,
) -> anyhow::Result<Option<StoredCommandOutcome>> {
    Ok(transaction
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
        }))
}

/// Answer a retry that is already settled, or `None` to run the command.
///
/// This runs BEFORE a composition that cannot be part of the ledger
/// transaction, so it commits and releases the lock. It is an optimization and
/// not the authority: the ledger insert re-reads under the lock, and the durable
/// idempotency keys inside the composition are what make a racing duplicate
/// converge.
async fn settle_retry(
    backend: &mut InternalAuthoringBackend,
    audit: &CommandAudit,
    request_hash: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    let (_, transaction) = backend.begin_command_transaction(audit.tenant_id()).await?;
    lock_retry_identity(&transaction, audit).await?;
    let existing = read_retry_outcome(&transaction, audit).await?;
    let settled = match classify_retry(existing.as_ref(), request_hash) {
        RetryDecision::Execute => None,
        RetryDecision::Replay => Some(
            existing
                .expect("replay requires a stored outcome")
                .outcome_bytes,
        ),
        RetryDecision::Reuse => Some(gate_command_id_reuse(&audit.command_id)?),
    };
    transaction
        .commit()
        .await
        .context("commit the retry classification read")?;
    Ok(settled)
}

fn gate_command_id_reuse(command_id: &str) -> anyhow::Result<Vec<u8>> {
    command_response_bytes(
        command_id,
        AuthoringOutcome::Refused(CommandRefusal::Gate(GateRefusal::CommandIdReuse)),
    )
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
    use wamn_authoring_model::{AuthoringCommandKind, QueryId};


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

    const SCOPE_ORG: &str = "acme";
    const SCOPE_PROJECT: &str = "receiving";
    const SCOPE_ENVIRONMENT: &str = "dev";
    const CONTROL_DATABASE: &str = "wamn-system";
    /// The T1 system database this fixture's identity read names.
    const SYSTEM_DATABASE: &str = "wamn-system";
    const PROJECT_DATABASE: &str = "wamn-db-acme--receiving--dev--k3m9x2p7";

    /// The scoped identity-reader login for this fixture's scope
    /// (`wamn-0h0g.12.67`). `SYSTEM_DATABASE` is the database the fixture's
    /// `WAMN_SYSTEM_URL` names, and it is inside the scope digest, so this is
    /// the ONLY user string that URL can carry and still be accepted.
    fn identity_reader_login(generation: wamn_control_provision::CredentialGeneration) -> String {
        wamn_control_provision::system_reader_generation_role(
            wamn_control_provision::SystemReader::Identity,
            SCOPE_ORG,
            SCOPE_PROJECT,
            SCOPE_ENVIRONMENT,
            SYSTEM_DATABASE,
            generation,
        )
    }

    /// `serve` arguments whose every input but the admission URL is in scope, so
    /// what the admission gate does is the only thing under test.
    ///
    /// The system URL names a reserved-TLD host that cannot resolve: whatever
    /// error escapes `serve` after the pure gates therefore identifies itself,
    /// and cannot be mistaken for one of them. Its USER, however, must be the
    /// scoped identity-reader generation, because that input is settled purely
    /// too.
    fn admission_probe_args(admission_url: &str) -> ManagementServeArgs {
        let authoring = wamn_control_provision::control_author_generation_role(
            SCOPE_ORG,
            SCOPE_PROJECT,
            SCOPE_ENVIRONMENT,
            CONTROL_DATABASE,
            wamn_control_provision::CredentialGeneration::A,
        );
        ManagementServeArgs {
            bind: "127.0.0.1:0".to_owned(),
            system_url: format!(
                "postgres://{}:secret@system.invalid:5432/{SYSTEM_DATABASE}",
                identity_reader_login(wamn_control_provision::CredentialGeneration::A)
            ),
            control_authoring_database_url: format!(
                "postgres://{authoring}:secret@control.invalid:5432/{CONTROL_DATABASE}"
            ),
            management_admission_database_url: admission_url.to_owned(),
            org: SCOPE_ORG.to_owned(),
            project: SCOPE_PROJECT.to_owned(),
            environment: SCOPE_ENVIRONMENT.to_owned(),
            tenant: "tenant-a".to_owned(),
            source_schema: "wamn_run".to_owned(),
        }
    }

    /// wamn-0h0g.8.5.3: the admission connection input is settled on the same
    /// terms as the authoring one, and by `serve` itself.
    ///
    /// This is the CALL-SITE proof. `wamn-control-provision` proves the parser;
    /// nothing there proves the production entry point runs it. Reaching an
    /// admission refusal means the authoring gate passed and the admission gate
    /// then ran — before the identity connect, whose failure against an
    /// unresolvable host is the error this test would see instead if the call
    /// were removed or moved down.
    #[tokio::test]
    async fn the_admission_connection_input_is_settled_before_any_io() {
        for out_of_scope in [
            // Absent: no fallback exists to pick up the slack.
            String::new(),
            // The shared query role this credential exists to replace.
            format!("postgres://wamn_app:secret@project.invalid:5432/{PROJECT_DATABASE}"),
            // The CONTROL database with an otherwise-shaped identity: the two
            // planes are separate connections, never one another's fallback.
            format!(
                "postgres://{}:secret@control.invalid:5432/{CONTROL_DATABASE}",
                wamn_control_provision::management_admitter_generation_role(
                    SCOPE_ORG,
                    SCOPE_PROJECT,
                    SCOPE_ENVIRONMENT,
                    PROJECT_DATABASE,
                    wamn_control_provision::CredentialGeneration::A,
                )
            ),
        ] {
            let error = serve(admission_probe_args(&out_of_scope))
                .await
                .expect_err("an out-of-scope admission input must refuse");
            let rendered = format!("{error}");
            assert!(
                rendered.contains("WAMN_MANAGEMENT_ADMISSION_PG_URL"),
                "{rendered}"
            );
            // A refusal names the variable, never the credential in it.
            assert!(!rendered.contains("secret"), "{rendered}");
        }

        // The accepting half: an in-scope generation passes the gate, so the
        // refusals above are the predicate working rather than the gate refusing
        // everything. `serve` then fails on the identity connect it was always
        // going to reach.
        let admitted = format!(
            "postgres://{}:secret@project.invalid:5432/{PROJECT_DATABASE}",
            wamn_control_provision::management_admitter_generation_role(
                SCOPE_ORG,
                SCOPE_PROJECT,
                SCOPE_ENVIRONMENT,
                PROJECT_DATABASE,
                wamn_control_provision::CredentialGeneration::B,
            )
        );
        let error = serve(admission_probe_args(&admitted))
            .await
            .expect_err("the unresolvable identity host still fails the startup");
        let rendered = format!("{error}");
        assert!(
            !rendered.contains("WAMN_MANAGEMENT_ADMISSION_PG_URL"),
            "an in-scope admission input was refused: {rendered}"
        );
        assert!(
            rendered.contains("connect the T1 system database for identity"),
            "{rendered}"
        );
    }


    /// THE FORGERY PRIMITIVE, CLOSED AT THE CALL SITE (`wamn-0h0g.12.67`).
    ///
    /// `wamn-system-db` authenticates as `wamn_system`, the AUTHORIZATION owner
    /// of the `identity` schema, and `identity.*` has NO row-level security. Any
    /// holder of that Secret could INSERT a token_hash/token_prefix pair bound to
    /// any principal and present the matching PAT to THIS surface, or INSERT an
    /// identity.project_roles row and self-grant in any org or project — this
    /// surface's whole authorization model is rows in a table that credential
    /// owns. The exposure was live the moment the manifest was applied.
    ///
    /// This is the CALL-SITE proof, the sibling of
    /// `the_admission_connection_input_is_settled_before_any_io`.
    /// `wamn-control-provision` proves the parser; nothing there proves the
    /// production entry point runs it. The accepting half then fails on the
    /// identity CONNECT — the error this test would see instead if the call were
    /// removed, which is what makes the refusals below the predicate working
    /// rather than the gate refusing everything.
    #[tokio::test]
    async fn the_identity_read_credential_is_settled_before_any_io() {
        for out_of_scope in [
            // Absent: no fallback exists to pick up the slack.
            String::new(),
            // THE WIDE OWNER. This is the credential the manifest carried.
            format!("postgres://wamn_system:secret@system.invalid:5432/{SYSTEM_DATABASE}"),
            // The control-author generation this same pod already mounts: a
            // separate plane, never this one's fallback.
            format!(
                "postgres://{}:secret@system.invalid:5432/{SYSTEM_DATABASE}",
                wamn_control_provision::control_author_generation_role(
                    SCOPE_ORG,
                    SCOPE_PROJECT,
                    SCOPE_ENVIRONMENT,
                    CONTROL_DATABASE,
                    wamn_control_provision::CredentialGeneration::A,
                )
            ),
            // The OTHER control reader's credential: the two grant sets are
            // disjoint, so its login must not satisfy this predicate either.
            format!(
                "postgres://{}:secret@system.invalid:5432/{SYSTEM_DATABASE}",
                wamn_control_provision::system_reader_generation_role(
                    wamn_control_provision::SystemReader::Registry,
                    SCOPE_ORG,
                    SCOPE_PROJECT,
                    SCOPE_ENVIRONMENT,
                    SYSTEM_DATABASE,
                    wamn_control_provision::CredentialGeneration::A,
                )
            ),
        ] {
            let mut args = admission_probe_args(&in_scope_admission_url());
            args.system_url = out_of_scope;
            let error = serve(args)
                .await
                .expect_err("an out-of-scope identity read input must refuse");
            let rendered = format!("{error}");
            assert!(rendered.contains("WAMN_SYSTEM_URL"), "{rendered}");
            // A refusal names the variable, never the credential in it.
            assert!(!rendered.contains("secret"), "{rendered}");
        }

        // Both of its OWN generations are accepted, so rotation is not a
        // refusal; `serve` then fails on the connect it was always going to
        // reach.
        for generation in [
            wamn_control_provision::CredentialGeneration::A,
            wamn_control_provision::CredentialGeneration::B,
        ] {
            let mut args = admission_probe_args(&in_scope_admission_url());
            args.system_url = format!(
                "postgres://{}:secret@system.invalid:5432/{SYSTEM_DATABASE}",
                identity_reader_login(generation)
            );
            let error = serve(args)
                .await
                .expect_err("the unresolvable identity host still fails the startup");
            let rendered = format!("{error}");
            assert!(
                !rendered.contains("WAMN_SYSTEM_URL"),
                "an in-scope identity read input was refused: {rendered}"
            );
            assert!(
                rendered.contains("connect the T1 system database for identity"),
                "{rendered}"
            );
        }
    }

    fn in_scope_admission_url() -> String {
        format!(
            "postgres://{}:secret@project.invalid:5432/{PROJECT_DATABASE}",
            wamn_control_provision::management_admitter_generation_role(
                SCOPE_ORG,
                SCOPE_PROJECT,
                SCOPE_ENVIRONMENT,
                PROJECT_DATABASE,
                wamn_control_provision::CredentialGeneration::A,
            )
        )
    }

    /// One management instance serves exactly one `(org, project, environment)`,
    /// so a client-selected environment is reconciled, never accepted.
    #[test]
    fn one_surface_serves_exactly_one_project_environment() {
        let admitted =
            reconcile_command_scope("tenant-a", "acme", "receiving", "dev", "receiving", "dev")
                .expect("the fixed scope is admitted");
        assert_eq!(admitted.tenant_id.as_ref(), "tenant-a");
        assert_eq!(admitted.org.as_ref(), "acme");
        assert_eq!(admitted.project.as_ref(), "receiving");
        assert_eq!(admitted.environment.as_ref(), "dev");

        for (project, environment) in [
            ("receiving", "prod"),
            ("receiving", "Dev"),
            ("receiving", ""),
            ("shipping", "dev"),
            ("", "dev"),
        ] {
            assert!(
                reconcile_command_scope(
                    "tenant-a",
                    "acme",
                    "receiving",
                    "dev",
                    project,
                    environment
                )
                .is_none(),
                "admitted {project:?}/{environment:?}"
            );
        }
    }

    #[test]
    fn ledger_command_literals_match_the_wire_contract_spelling() {
        for (kind, audited) in [
            (AuthoringCommandKind::Gate, AuditedCommand::Gate),
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
    fn canonical_request_hash_ignores_object_order_and_detects_content_change() {
        let request = |input: serde_json::Value| {
            serde_json::from_value::<AuthoringRequest>(serde_json::json!({
                "schema-version": "0.1",
                "command-id": "retry-1",
                "command": {
                    "kind": "test-set-run",
                    "input": {
                        "scope": input,
                        "validated-draft": {"validated-draft-id": "validated-1"}
                    }
                }
            }))
            .unwrap()
        };
        let left = request(serde_json::json!({"project-id": "p", "environment": "dev"}));
        let reordered = request(serde_json::json!({"environment": "dev", "project-id": "p"}));
        let changed = request(serde_json::json!({"project-id": "p", "environment": "prod"}));
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


    /// The mounted inventory, and the shape of the answer an unmounted kind
    /// gets, decided by CALLING route selection over every contract kind.
    ///
    /// This replaces a source scan that counted a substring of its own file
    /// (wamn-0h0g.8.5.4). Nothing here reads `management.rs`: it builds one
    /// command per kind, evaluates [`command_route`] — the same function
    /// `dispatch_command` matches on, and its only route decision — and
    /// evaluates the actual unmounted response. A kind added to the contract
    /// fails to compile here until it is classified, and a kind quietly mounted
    /// or unmounted changes an answer this test reads.
    ///
    /// The remaining property, that route selection happens AFTER
    /// authorization, is not decidable from route selection alone. It is pinned
    /// by `identity_is_settled_before_the_request_body_is_read` above (the
    /// refusal precedes the body read, and a command cannot be decoded — let
    /// alone routed — before its body is read) and proved behaviourally by the
    /// live gate, where every untrusted presenter receives the identical `403`
    /// document for a mounted and an unmounted kind alike.
    #[test]
    fn only_the_mounted_kinds_have_a_route_and_the_rest_answer_a_bare_501() {
        use wamn_authoring_model::{
            AuthoringScope, Gate, PublishValidatedDraft, ValidatedDraftRef,
        };

        let scope = AuthoringScope {
            project_id: "receiving".to_owned(),
            environment: "dev".to_owned(),
        };
        let validated = ValidatedDraftRef {
            validated_draft_id: "sha256:".to_owned() + &"0".repeat(64),
        };
        let inventory = [
            (
                AuthoringCommandKind::Gate,
                AuthoringCommand::Gate(Gate {
                    scope: scope.clone(),
                    validated_draft: validated.clone(),
                }),
            ),
            (
                AuthoringCommandKind::Publish,
                AuthoringCommand::Publish(PublishValidatedDraft {
                    scope,
                    validated_draft: validated,
                    successful_report_id: "report".to_owned(),
                }),
            ),
        ];
        // Every kind the contract declares is exercised, so the inventory cannot
        // silently omit one. wamn-0h0g.8.5.5 collapsed five commands to two.
        assert_eq!(inventory.len(), 2);

        let mut mounted = Vec::new();
        for (kind, command) in &inventory {
            match command_route(command) {
                CommandRoute::Unmounted => {
                    let response = route_absent();
                    assert_eq!(
                        response.status(),
                        StatusCode::NOT_IMPLEMENTED,
                        "{kind:?} answers an unmounted kind with something other than 501"
                    );
                    // The absence of a route carries nothing: no body bytes and
                    // no content type a client could read a document out of.
                    assert_eq!(
                        hyper::body::Body::size_hint(response.body()).exact(),
                        Some(0),
                        "{kind:?} answered 501 carrying a document"
                    );
                    assert!(
                        response
                            .headers()
                            .get(hyper::header::CONTENT_TYPE)
                            .is_none(),
                        "{kind:?} answered 501 typed as a document"
                    );
                }
                routed => mounted.push((*kind, format!("{routed:?}"))),
            }
        }
        let mounted_kinds: Vec<AuthoringCommandKind> =
            mounted.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(
            mounted_kinds,
            [AuthoringCommandKind::Gate],
            "the mounted inventory changed"
        );
        // A mounted kind is routed to its OWN handler input, not merely to
        // something that is not `Unmounted`.
        assert!(mounted[0].1.starts_with("Gate("));
    }



}
