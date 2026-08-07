//! Privileged operator surface for uncertain `never-replay` effects.
//!
//! This is deliberately a platform break-glass command, not the project
//! adapter. The database session supplies the audit principal through
//! `SESSION_USER`; command arguments can never select or impersonate it.

use anyhow::{Context as _, ensure};
use clap::{Args, ValueEnum};
use serde::Serialize;
use serde_json::Value;
use tokio_postgres::{IsolationLevel, NoTls, error::SqlState, types::ToSql};
use wamn_run_state::disposition::{
    BulkDisposition, BulkSelector, DispositionAction, ResolutionAudit, ResolutionBasis,
    ResolutionOutcome, ResolvedFailureKind, SingleDisposition, platform_break_glass_bulk_sql,
    platform_break_glass_single_sql, validate_platform_bulk, validate_platform_single,
    select_run_dispositions_sql,
};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DispositionActionArg {
    Park,
    Release,
    Resolve,
}

impl From<DispositionActionArg> for DispositionAction {
    fn from(value: DispositionActionArg) -> Self {
        match value {
            DispositionActionArg::Park => Self::Park,
            DispositionActionArg::Release => Self::Release,
            DispositionActionArg::Resolve => Self::Resolve,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ResolutionBasisArg {
    ExternalEvidence,
    CounterpartyConfirmation,
    OperatorJudgment,
}

impl From<ResolutionBasisArg> for ResolutionBasis {
    fn from(value: ResolutionBasisArg) -> Self {
        match value {
            ResolutionBasisArg::ExternalEvidence => Self::ExternalEvidence,
            ResolutionBasisArg::CounterpartyConfirmation => Self::CounterpartyConfirmation,
            ResolutionBasisArg::OperatorJudgment => Self::OperatorJudgment,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ResolutionStatusArg {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum FailureKindArg {
    Terminal,
    InvalidInput,
}

impl From<FailureKindArg> for ResolvedFailureKind {
    fn from(value: FailureKindArg) -> Self {
        match value {
            FailureKindArg::Terminal => Self::Terminal,
            FailureKindArg::InvalidInput => Self::InvalidInput,
        }
    }
}

/// Apply one explicitly privileged effect disposition. Exactly one of
/// `--attempt-id` (single) or `--connection-name` (bounded bulk) is required.
#[derive(Debug, Args)]
#[command(group(
    clap::ArgGroup::new("target")
        .required(true)
        .multiple(false)
        .args(["attempt_id", "connection_name"])
))]
pub struct EffectDispositionBreakGlassArgs {
    /// Superuser project database URL. Its login becomes the immutable actor.
    #[arg(long, env = "WAMN_ADMIN_DATABASE_URL")]
    pub admin_database_url: String,

    /// Run-plane schema in the project database.
    #[arg(long, default_value = "wamn_run")]
    pub schema: String,

    /// Tenant claim applied transaction-locally.
    #[arg(long)]
    pub tenant: String,

    #[arg(long, value_enum)]
    pub action: DispositionActionArg,

    /// Target one immutable attempt UUID.
    #[arg(long)]
    pub attempt_id: Option<String>,

    /// Start a bounded bulk selector.
    #[arg(long)]
    pub connection_name: Option<String>,

    #[arg(long)]
    pub connection_generation: Option<String>,

    /// Inclusive PostgreSQL timestamptz start for a bulk selector.
    #[arg(long)]
    pub window_start: Option<String>,

    /// Exclusive PostgreSQL timestamptz end for a bulk selector.
    #[arg(long)]
    pub window_end: Option<String>,

    /// Optional flow narrowing for a bulk selector.
    #[arg(long)]
    pub flow_id: Option<String>,

    #[arg(long)]
    pub correlation_id: String,

    /// Mandatory platform break-glass justification.
    #[arg(long)]
    pub reason: String,

    #[arg(long, value_enum)]
    pub basis: Option<ResolutionBasisArg>,

    #[arg(long)]
    pub evidence_ref: Option<String>,

    #[arg(long, value_enum)]
    pub resolution_status: Option<ResolutionStatusArg>,

    /// Complete asserted success payload as JSON.
    #[arg(long)]
    pub success_payload: Option<String>,

    /// Explicit output port from the pinned node interface.
    #[arg(long)]
    pub success_port: Option<String>,

    /// Optional whole context replacement as a JSON object.
    #[arg(long)]
    pub success_context: Option<String>,

    #[arg(long, value_enum)]
    pub failure_kind: Option<FailureKindArg>,

    /// Typed error detail JSON (`message`, optional `code` and `data`).
    #[arg(long)]
    pub failure_detail: Option<String>,
}

/// Read the immutable disposition projection for every effect attempt in one
/// run. This is ordinary tenant-scoped read authority, not a disposition verb.
#[derive(Debug, Args)]
pub struct EffectDispositionViewArgs {
    #[arg(long, env = "WAMN_DATABASE_URL")]
    pub database_url: String,

    /// Run-plane schema in the project database.
    #[arg(long, default_value = "wamn_run")]
    pub schema: String,

    #[arg(long)]
    pub tenant: String,

    #[arg(long)]
    pub run_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
struct EffectDispositionView {
    attempt_id: String,
    node_id: String,
    occurrence: i32,
    connection_name: Option<String>,
    connection_generation: Option<String>,
    verified_author_principal: Option<String>,
    verified_publisher_principal: Option<String>,
    disposition_state: String,
    resolution_status: Option<String>,
    principal: Option<String>,
    effective_role: Option<String>,
    basis: Option<String>,
    evidence_ref: Option<String>,
    correlation_id: Option<String>,
    break_glass_reason: Option<String>,
}

struct ResolutionFields {
    basis: Option<String>,
    evidence_ref: Option<String>,
    status: Option<String>,
    success_payload: Option<String>,
    success_port: Option<String>,
    success_context: Option<String>,
    failure_kind: Option<String>,
    failure_detail: Option<String>,
}

enum PreparedDisposition {
    Single(SingleDisposition),
    Bulk(BulkDisposition),
}

const SERIALIZATION_ATTEMPTS: usize = 4;

pub async fn run(args: EffectDispositionBreakGlassArgs) -> anyhow::Result<()> {
    ensure!(!args.tenant.is_empty(), "tenant-required");
    ensure!(!args.reason.is_empty(), "break-glass-reason-required");
    let schema = wamn_schema_control::BareSchemaName::new(args.schema.clone())
        .with_context(|| format!("invalid --schema {:?}", args.schema))?;
    let schema_name = schema.as_str().to_string();

    let action = DispositionAction::from(args.action);
    let audit = build_audit(&args)?;
    let outcome = build_outcome(&args)?;
    let fields = resolution_fields(audit.as_ref(), outcome.as_ref());

    let request = if let Some(attempt_id) = args.attempt_id.as_ref() {
        ensure!(
            args.connection_generation.is_none()
                && args.window_start.is_none()
                && args.window_end.is_none()
                && args.flow_id.is_none(),
            "bulk-selector-not-permitted-with-attempt-id"
        );
        let request = SingleDisposition {
            attempt_id: attempt_id.clone(),
            action,
            correlation_id: args.correlation_id.clone(),
            audit,
            outcome,
        };
        validate_platform_single(&request).context("invalid platform disposition")?;
        PreparedDisposition::Single(request)
    } else {
        let selector = BulkSelector {
            connection_name: args.connection_name.clone().unwrap_or_default(),
            connection_generation: args.connection_generation.clone().unwrap_or_default(),
            window_start: args.window_start.clone().unwrap_or_default(),
            window_end: args.window_end.clone().unwrap_or_default(),
            flow_id: args.flow_id.clone(),
        };
        let request = BulkDisposition {
            selector,
            action,
            correlation_id: args.correlation_id.clone(),
            audit,
            outcome,
        };
        validate_platform_bulk(&request).context("invalid platform disposition")?;
        PreparedDisposition::Bulk(request)
    };

    let (mut client, connection) = tokio_postgres::connect(&args.admin_database_url, NoTls)
        .await
        .context("connect privileged project database")?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::error!(%error, "effect-disposition database connection failed");
        }
    });

    for attempt in 0..SERIALIZATION_ATTEMPTS {
        let transaction = client
            .build_transaction()
            .isolation_level(IsolationLevel::Serializable)
            .start()
            .await
            .context("begin serializable effect-disposition transaction")?;
        transaction
            .execute(
                "SELECT set_config('search_path', $1, true)",
                &[&schema_name],
            )
            .await
            .context("set disposition search path")?;
        transaction
            .execute("SELECT set_config('app.tenant', $1, true)", &[&args.tenant])
            .await
            .context("set tenant claim")?;

        let applied = match &request {
            PreparedDisposition::Single(request) => {
                execute_single(&transaction, request, &fields, &args.reason).await
            }
            PreparedDisposition::Bulk(request) => {
                execute_bulk(&transaction, request, &fields, &args.reason).await
            }
        };
        let result = match applied {
            Ok(result) => result,
            Err(error) if is_serialization_failure(&error) => {
                let _ = transaction.rollback().await;
                if attempt + 1 < SERIALIZATION_ATTEMPTS {
                    continue;
                }
                return Err(error).context("effect disposition serialization retries exhausted");
            }
            Err(error) => {
                let _ = transaction.rollback().await;
                return Err(error);
            }
        };

        if result.code != "applied" {
            transaction.rollback().await.context("rollback refusal")?;
            anyhow::bail!("effect disposition refused: {}", result.code);
        }
        let request_id = result
            .request_id
            .clone()
            .context("applied disposition omitted request id")?;
        match transaction.commit().await {
            Ok(()) => {
                println!(
                    "effect-disposition applied request_id={} selection_count={}",
                    request_id, result.selection_count
                );
                return Ok(());
            }
            Err(error)
                if error.code() == Some(&SqlState::T_R_SERIALIZATION_FAILURE)
                    && attempt + 1 < SERIALIZATION_ATTEMPTS =>
            {
                continue;
            }
            Err(error) if error.code() == Some(&SqlState::T_R_SERIALIZATION_FAILURE) => {
                return Err(error)
                    .context("effect disposition serialization retries exhausted");
            }
            Err(error) => return Err(error).context("commit effect disposition"),
        }
    }
    unreachable!("bounded serializable retry loop always returns")
}

pub async fn view(args: EffectDispositionViewArgs) -> anyhow::Result<()> {
    ensure!(!args.tenant.is_empty(), "tenant-required");
    ensure!(!args.run_id.is_empty(), "run-id-required");
    let schema = wamn_schema_control::BareSchemaName::new(args.schema.clone())
        .with_context(|| format!("invalid --schema {:?}", args.schema))?;
    let schema_name = schema.as_str().to_string();
    let (mut client, connection) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("connect project database")?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::error!(%error, "effect-disposition view connection failed");
        }
    });
    let transaction = client
        .transaction()
        .await
        .context("begin effect-disposition view")?;
    transaction
        .batch_execute("SET TRANSACTION READ ONLY")
        .await
        .context("set read-only disposition view")?;
    transaction
        .execute(
            "SELECT set_config('search_path', $1, true)",
            &[&schema_name],
        )
        .await
        .context("set disposition view search path")?;
    transaction
        .execute("SELECT set_config('app.tenant', $1, true)", &[&args.tenant])
        .await
        .context("set tenant claim")?;
    let rows = transaction
        .query(select_run_dispositions_sql(), &[&args.run_id])
        .await
        .context("read effect dispositions")?;
    let views: Vec<EffectDispositionView> = rows
        .into_iter()
        .map(|row| EffectDispositionView {
            attempt_id: row.get(0),
            node_id: row.get(1),
            occurrence: row.get(2),
            connection_name: row.get(3),
            connection_generation: row.get(4),
            verified_author_principal: row.get(5),
            verified_publisher_principal: row.get(6),
            disposition_state: row.get(7),
            resolution_status: row.get(8),
            principal: row.get(9),
            effective_role: row.get(10),
            basis: row.get(11),
            evidence_ref: row.get(12),
            correlation_id: row.get(13),
            break_glass_reason: row.get(14),
        })
        .collect();
    transaction
        .commit()
        .await
        .context("finish effect-disposition view")?;
    println!("{}", serde_json::to_string_pretty(&views)?);
    Ok(())
}

fn build_audit(args: &EffectDispositionBreakGlassArgs) -> anyhow::Result<Option<ResolutionAudit>> {
    match (args.basis, args.evidence_ref.as_ref()) {
        (None, None) => Ok(None),
        (Some(basis), Some(evidence_ref)) => Ok(Some(ResolutionAudit {
            basis: basis.into(),
            evidence_ref: evidence_ref.clone(),
        })),
        _ => anyhow::bail!("resolution-audit-required"),
    }
}

fn build_outcome(
    args: &EffectDispositionBreakGlassArgs,
) -> anyhow::Result<Option<ResolutionOutcome>> {
    let any_success = args.success_payload.is_some()
        || args.success_port.is_some()
        || args.success_context.is_some();
    let any_failure = args.failure_kind.is_some() || args.failure_detail.is_some();
    match args.resolution_status {
        None => {
            ensure!(!any_success && !any_failure, "resolution-status-required");
            Ok(None)
        }
        Some(ResolutionStatusArg::Succeeded) => {
            ensure!(!any_failure, "failure-fields-not-permitted");
            let payload = parse_required_json("success-payload", &args.success_payload)?;
            let port = args.success_port.clone().unwrap_or_default();
            let context = parse_optional_json("success-context", &args.success_context)?;
            Ok(Some(ResolutionOutcome::Succeeded {
                payload,
                port,
                context,
            }))
        }
        Some(ResolutionStatusArg::Failed) => {
            ensure!(!any_success, "success-fields-not-permitted");
            let kind = args.failure_kind.context("failure-kind-required")?.into();
            let detail = parse_required_json("failure-detail", &args.failure_detail)?;
            Ok(Some(ResolutionOutcome::Failed { kind, detail }))
        }
    }
}

fn parse_required_json(label: &str, value: &Option<String>) -> anyhow::Result<Value> {
    let value = value
        .as_ref()
        .with_context(|| format!("{label}-required"))?;
    serde_json::from_str(value).with_context(|| format!("invalid-{label}-json"))
}

fn parse_optional_json(label: &str, value: &Option<String>) -> anyhow::Result<Option<Value>> {
    value
        .as_ref()
        .map(|value| serde_json::from_str(value).with_context(|| format!("invalid-{label}-json")))
        .transpose()
}

fn resolution_fields(
    audit: Option<&ResolutionAudit>,
    outcome: Option<&ResolutionOutcome>,
) -> ResolutionFields {
    let (status, success_payload, success_port, success_context, failure_kind, failure_detail) =
        match outcome {
            Some(ResolutionOutcome::Succeeded {
                payload,
                port,
                context,
            }) => (
                Some("succeeded".to_string()),
                Some(payload.to_string()),
                Some(port.clone()),
                context.as_ref().map(Value::to_string),
                None,
                None,
            ),
            Some(ResolutionOutcome::Failed { kind, detail }) => (
                Some("failed".to_string()),
                None,
                None,
                None,
                Some(kind.as_sql().to_string()),
                Some(detail.to_string()),
            ),
            None => (None, None, None, None, None, None),
        };
    ResolutionFields {
        basis: audit.map(|audit| audit.basis.as_sql().to_string()),
        evidence_ref: audit.map(|audit| audit.evidence_ref.clone()),
        status,
        success_payload,
        success_port,
        success_context,
        failure_kind,
        failure_detail,
    }
}

struct AppliedResult {
    code: String,
    request_id: Option<String>,
    selection_count: i64,
}

fn is_serialization_failure(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<tokio_postgres::Error>()
        .is_some_and(|error| error.code() == Some(&SqlState::T_R_SERIALIZATION_FAILURE))
}

async fn execute_single(
    transaction: &tokio_postgres::Transaction<'_>,
    request: &SingleDisposition,
    fields: &ResolutionFields,
    reason: &str,
) -> anyhow::Result<AppliedResult> {
    let action = request.action.as_sql().to_string();
    let ignored_actor: Option<String> = None;
    let params: [&(dyn ToSql + Sync); 14] = [
        &request.attempt_id,
        &action,
        &ignored_actor,
        &ignored_actor,
        &fields.basis,
        &fields.evidence_ref,
        &request.correlation_id,
        &fields.status,
        &fields.success_payload,
        &fields.success_port,
        &fields.success_context,
        &fields.failure_kind,
        &fields.failure_detail,
        &reason,
    ];
    let row = transaction
        .query_one(platform_break_glass_single_sql(), &params)
        .await
        .context("apply single effect disposition")?;
    Ok(AppliedResult {
        code: row.get(0),
        request_id: row.get(1),
        selection_count: 1,
    })
}

async fn execute_bulk(
    transaction: &tokio_postgres::Transaction<'_>,
    request: &BulkDisposition,
    fields: &ResolutionFields,
    reason: &str,
) -> anyhow::Result<AppliedResult> {
    let action = request.action.as_sql().to_string();
    let ignored_actor: Option<String> = None;
    let params: [&(dyn ToSql + Sync); 18] = [
        &request.selector.connection_name,
        &request.selector.connection_generation,
        &request.selector.window_start,
        &request.selector.window_end,
        &request.selector.flow_id,
        &action,
        &ignored_actor,
        &ignored_actor,
        &fields.basis,
        &fields.evidence_ref,
        &request.correlation_id,
        &fields.status,
        &fields.success_payload,
        &fields.success_port,
        &fields.success_context,
        &fields.failure_kind,
        &fields.failure_detail,
        &reason,
    ];
    let row = transaction
        .query_one(&platform_break_glass_bulk_sql(), &params)
        .await
        .context("apply bounded bulk effect disposition")?;
    Ok(AppliedResult {
        code: row.get(0),
        request_id: row.get(1),
        selection_count: row.get(2),
    })
}

#[cfg(test)]
mod tests {
    use super::SERIALIZATION_ATTEMPTS;

    #[test]
    fn break_glass_uses_bounded_serializable_retries() {
        let source = include_str!("effect_disposition.rs");
        assert!(SERIALIZATION_ATTEMPTS > 1);
        assert!(source.contains("IsolationLevel::Serializable"));
        assert!(source.contains("SqlState::T_R_SERIALIZATION_FAILURE"));
        assert!(source.contains("serialization retries exhausted"));
    }
}
