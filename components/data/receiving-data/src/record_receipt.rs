//! Atomic `receiving.record_receipt` command over generated static accessors.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use chrono::{DateTime, SecondsFormat};
use serde_json::{Value, json};
use sqlx_core::error::Error as SqlxError;
use sqlx_core::transaction::Transaction;
use uuid::Uuid;
use wamn_execution_contract::canonical_json_bytes;
use wamn_postgres_sqlx::{
    Json, TimestampTz, Uuid as WamnUuid, WamnConnection, WamnPostgres, run_transaction,
};

use crate::error::{AccessError, AccessErrorKind, AllowedConstraints};
use crate::generated::wamn::receiving_record_receipt as generated;

/// Maximum number of independently transacted items in one operation envelope.
pub const MAX_RECORD_RECEIPT_ITEMS: usize = 100;
/// Maximum number of receipt facts in one command item.
pub const MAX_RECORD_RECEIPT_LINES: usize = 100;
/// Raw request ceiling enforced by ingress before JSON parsing.
pub const MAX_RECORD_RECEIPT_BODY_BYTES: usize = 1_048_576;

/// One array-envelope input item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordReceiptInput {
    pub request_id: Box<str>,
    pub value: RecordReceiptValue,
}

/// Authoritative receipt command payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordReceiptValue {
    pub idempotency_key: Box<str>,
    pub purchase_order_id: Box<str>,
    pub receipt_reference: Box<str>,
    pub occurred_at: Box<str>,
    pub line: Box<[RecordReceiptLine]>,
}

/// One received quantity and location fact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordReceiptLine {
    pub purchase_order_line_id: Box<str>,
    pub quantity: Box<str>,
    pub location_id: Box<str>,
}

/// Immutable result stored with the canonical command identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordReceiptResult {
    pub receipt_id: Box<str>,
    pub purchase_order_id: Box<str>,
    pub purchase_order_status: PurchaseOrderStatus,
    pub row_version: i64,
}

/// Closed status vocabulary returned by the command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PurchaseOrderStatus {
    Open,
    Complete,
}

impl PurchaseOrderStatus {
    /// Frozen serialized result value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Complete => "complete",
        }
    }

    fn parse(value: &str) -> Result<Self, RecordReceiptError> {
        match value {
            "open" => Ok(Self::Open),
            "complete" => Ok(Self::Complete),
            _ => Err(RecordReceiptError::internal(
                "database returned a status outside the command result contract",
            )),
        }
    }
}

/// One independently committed or refused envelope item.
#[derive(Debug)]
pub enum RecordReceiptItemOutcome {
    Succeeded {
        request_id: Box<str>,
        value: RecordReceiptResult,
    },
    Refused {
        request_id: Box<str>,
        error: RecordReceiptError,
    },
}

/// Stable command-level refusal class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordReceiptErrorKind {
    InvalidInput,
    PurchaseOrderNotFound,
    PurchaseOrderNotOpen,
    PurchaseOrderLineNotFound,
    PurchaseOrderLineMismatch,
    LocationNotFound,
    QuantityExceedsRemaining,
    ReceiptReferenceConflict,
    IdempotencyConflict,
    Retry,
    Timeout,
    PermissionDenied,
    InternalError,
}

impl RecordReceiptErrorKind {
    /// Frozen manifest-owned error literal.
    pub const fn literal(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::PurchaseOrderNotFound => "purchase_order_not_found",
            Self::PurchaseOrderNotOpen => "purchase_order_not_open",
            Self::PurchaseOrderLineNotFound => "purchase_order_line_not_found",
            Self::PurchaseOrderLineMismatch => "purchase_order_line_mismatch",
            Self::LocationNotFound => "location_not_found",
            Self::QuantityExceedsRemaining => "quantity_exceeds_remaining",
            Self::ReceiptReferenceConflict => "receipt_reference_conflict",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::Retry => "retry",
            Self::Timeout => "timeout",
            Self::PermissionDenied => "permission_denied",
            Self::InternalError => "internal_error",
        }
    }
}

/// Contextual command failure translated once by the owning operation adapter.
#[derive(Debug)]
pub struct RecordReceiptError {
    kind: RecordReceiptErrorKind,
    context: Box<str>,
    field: Option<&'static str>,
    id: Option<Box<str>>,
    minimum: Option<usize>,
    maximum: Option<usize>,
    observed: Option<usize>,
    constraint: Option<&'static str>,
    source: Option<Box<dyn Error + Send + Sync + 'static>>,
}

impl RecordReceiptError {
    /// Stable class; callers must not match display text.
    pub const fn kind(&self) -> RecordReceiptErrorKind {
        self.kind
    }

    /// Non-wire diagnostic for logs and tests.
    pub fn context(&self) -> &str {
        &self.context
    }

    /// Field owned by a command-domain refusal.
    pub const fn field(&self) -> Option<&'static str> {
        self.field
    }

    /// Exact offending identity owned by a line-domain refusal.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Optional lower input bound.
    pub const fn minimum(&self) -> Option<usize> {
        self.minimum
    }

    /// Optional upper input bound.
    pub const fn maximum(&self) -> Option<usize> {
        self.maximum
    }

    /// Optional observed input bound.
    pub const fn observed(&self) -> Option<usize> {
        self.observed
    }

    /// Exact named constraint owned by a command refusal.
    pub const fn constraint(&self) -> Option<&'static str> {
        self.constraint
    }

    fn new(kind: RecordReceiptErrorKind, context: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            context: context.into(),
            field: None,
            id: None,
            minimum: None,
            maximum: None,
            observed: None,
            constraint: None,
            source: None,
        }
    }

    fn invalid(context: impl Into<Box<str>>, field: &'static str) -> Self {
        let mut error = Self::new(RecordReceiptErrorKind::InvalidInput, context);
        error.field = Some(field);
        error
    }

    fn invalid_range(
        context: impl Into<Box<str>>,
        field: &'static str,
        minimum: usize,
        maximum: usize,
        observed: usize,
    ) -> Self {
        let mut error = Self::invalid(context, field);
        error.minimum = Some(minimum);
        error.maximum = Some(maximum);
        error.observed = Some(observed);
        error
    }

    fn internal(context: impl Into<Box<str>>) -> Self {
        Self::new(RecordReceiptErrorKind::InternalError, context)
    }

    fn from_sqlx(
        context: &'static str,
        source: SqlxError,
        allowed_constraints: AllowedConstraints,
    ) -> Self {
        let classified = AccessError::from_sqlx(context, &source, allowed_constraints);
        Self::from_classified_sqlx(classified.kind(), context, source)
    }

    fn from_classified_sqlx(
        classified: AccessErrorKind,
        context: &'static str,
        source: SqlxError,
    ) -> Self {
        let kind = match classified {
            AccessErrorKind::Retry => RecordReceiptErrorKind::Retry,
            AccessErrorKind::Timeout => RecordReceiptErrorKind::Timeout,
            AccessErrorKind::PermissionDenied => RecordReceiptErrorKind::PermissionDenied,
            AccessErrorKind::InvalidInput
            | AccessErrorKind::NotFound
            | AccessErrorKind::ConcurrencyConflict
            | AccessErrorKind::UniqueViolation
            | AccessErrorKind::ForeignKeyViolation
            | AccessErrorKind::CheckViolation
            | AccessErrorKind::InternalError => RecordReceiptErrorKind::InternalError,
        };
        Self {
            kind,
            context: context.into(),
            field: None,
            id: None,
            minimum: None,
            maximum: None,
            observed: None,
            constraint: None,
            source: Some(Box::new(source)),
        }
    }

    fn with_constraint_source(
        kind: RecordReceiptErrorKind,
        context: &'static str,
        constraint: &'static str,
        source: SqlxError,
    ) -> Self {
        Self {
            kind,
            context: context.into(),
            field: None,
            id: None,
            minimum: None,
            maximum: None,
            observed: None,
            constraint: Some(constraint),
            source: Some(Box::new(source)),
        }
    }

    fn domain(
        kind: RecordReceiptErrorKind,
        context: impl Into<Box<str>>,
        field: &'static str,
    ) -> Self {
        let mut error = Self::new(kind, context);
        error.field = Some(field);
        error
    }

    fn domain_id(
        kind: RecordReceiptErrorKind,
        context: impl Into<Box<str>>,
        field: &'static str,
        id: impl Into<Box<str>>,
    ) -> Self {
        let mut error = Self::domain(kind, context, field);
        error.id = Some(id.into());
        error
    }
}

impl fmt::Display for RecordReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.kind.literal(), self.context)
    }
}

impl Error for RecordReceiptError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

impl From<SqlxError> for RecordReceiptError {
    fn from(source: SqlxError) -> Self {
        Self::from_sqlx(
            "run record_receipt transaction",
            source,
            AllowedConstraints::NONE,
        )
    }
}

/// Execute each array item independently after enforcing the outer count bound.
pub async fn record_receipt(
    connection: &mut WamnConnection,
    input: &[RecordReceiptInput],
) -> Result<Box<[RecordReceiptItemOutcome]>, RecordReceiptError> {
    validate_count(
        "record_receipt item",
        "input",
        input.len(),
        MAX_RECORD_RECEIPT_ITEMS,
    )?;
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let result = record_receipt_item(connection, item).await;
        output.push(with_request_id(&item.request_id, result));
    }
    Ok(output.into_boxed_slice())
}

fn with_request_id(
    request_id: &str,
    result: Result<RecordReceiptResult, RecordReceiptError>,
) -> RecordReceiptItemOutcome {
    let request_id = request_id.into();
    match result {
        Ok(value) => RecordReceiptItemOutcome::Succeeded { request_id, value },
        Err(error) => RecordReceiptItemOutcome::Refused { request_id, error },
    }
}

/// Execute one item in exactly one transaction with no automatic retry.
async fn record_receipt_item(
    connection: &mut WamnConnection,
    command: &RecordReceiptInput,
) -> Result<RecordReceiptResult, RecordReceiptError> {
    let prepared = prepare(command)?;
    run_transaction(connection, move |transaction| {
        Box::pin(record_receipt_in(transaction, prepared))
    })
    .await
}

#[derive(Debug)]
struct PreparedCommand {
    idempotency_key: String,
    purchase_order_id: String,
    receipt_reference: String,
    occurred_at: String,
    canonical_command: Vec<u8>,
    line_json: String,
    line_count: usize,
}

fn prepare(command: &RecordReceiptInput) -> Result<PreparedCommand, RecordReceiptError> {
    if command.value.idempotency_key.is_empty() {
        return Err(RecordReceiptError::invalid(
            "idempotency_key must not be empty",
            "value.idempotency_key",
        ));
    }
    if command.value.receipt_reference.is_empty() {
        return Err(RecordReceiptError::invalid(
            "receipt_reference must not be empty",
            "value.receipt_reference",
        ));
    }
    validate_count(
        "record_receipt line",
        "value.line",
        command.value.line.len(),
        MAX_RECORD_RECEIPT_LINES,
    )?;
    let purchase_order_id =
        canonical_uuid(&command.value.purchase_order_id, "value.purchase_order_id")?;
    let occurred_at = canonical_timestamp(&command.value.occurred_at)?;

    let mut seen = BTreeSet::new();
    let mut lines = Vec::with_capacity(command.value.line.len());
    for line in &command.value.line {
        let purchase_order_line_id = canonical_uuid(
            &line.purchase_order_line_id,
            "value.line[].purchase_order_line_id",
        )?;
        if !seen.insert(purchase_order_line_id) {
            return Err(RecordReceiptError::invalid(
                format!("duplicate purchase_order_line_id {purchase_order_line_id}"),
                "value.line[].purchase_order_line_id",
            ));
        }
        let location_id = canonical_uuid(&line.location_id, "value.line[].location_id")?;
        validate_positive_numeric(&line.quantity)?;
        lines.push((purchase_order_line_id, line.quantity.as_ref(), location_id));
    }
    lines.sort_by_key(|line| line.0);
    let line = lines
        .iter()
        .map(|(line_id, quantity, location_id)| {
            json!({
                "purchase_order_line_id": line_id.hyphenated().to_string(),
                "quantity": quantity,
                "location_id": location_id.hyphenated().to_string(),
            })
        })
        .collect::<Vec<_>>();
    let line_value = Value::Array(line);
    let canonical_value = json!({
        "purchase_order_id": purchase_order_id.hyphenated().to_string(),
        "receipt_reference": command.value.receipt_reference,
        "occurred_at": occurred_at,
        "line": line_value,
    });
    let line_json = String::from_utf8(canonical_json_bytes(&canonical_value["line"]))
        .expect("canonical JSON is UTF-8");
    Ok(PreparedCommand {
        idempotency_key: command.value.idempotency_key.to_string(),
        purchase_order_id: purchase_order_id.hyphenated().to_string(),
        receipt_reference: command.value.receipt_reference.to_string(),
        occurred_at,
        canonical_command: canonical_json_bytes(&canonical_value),
        line_json,
        line_count: command.value.line.len(),
    })
}

async fn record_receipt_in(
    transaction: &mut Transaction<'_, WamnPostgres>,
    command: PreparedCommand,
) -> Result<RecordReceiptResult, RecordReceiptError> {
    if let Some(replay) = generated::find_replay(transaction, command.idempotency_key.clone())
        .await
        .map_err(|source| sql_error("find record_receipt replay", source))?
    {
        return replay_result(replay, &command.canonical_command);
    }

    let claim = generated::claim_command(
        transaction,
        command.idempotency_key.clone(),
        command.canonical_command.clone(),
        WamnUuid(command.purchase_order_id.clone()),
    )
    .await
    .map_err(|source| sql_error("claim record_receipt idempotency key", source))?;
    let Some(claim) = claim else {
        let replay = generated::find_replay(transaction, command.idempotency_key.clone())
            .await
            .map_err(|source| sql_error("load concurrent record_receipt replay", source))?
            .ok_or_else(|| {
                RecordReceiptError::internal("conflicting command claim has no durable result")
            })?;
        return replay_result(replay, &command.canonical_command);
    };

    let purchase_order =
        generated::lock_purchase_order(transaction, WamnUuid(command.purchase_order_id.clone()))
            .await
            .map_err(|source| sql_error("lock purchase_order", source))?
            .ok_or_else(|| {
                RecordReceiptError::domain(
                    RecordReceiptErrorKind::PurchaseOrderNotFound,
                    "purchase_order does not exist",
                    "value.purchase_order_id",
                )
            })?;
    if purchase_order.status != "open" {
        return Err(RecordReceiptError::domain(
            RecordReceiptErrorKind::PurchaseOrderNotOpen,
            "purchase_order is not open",
            "value.purchase_order_id",
        ));
    }

    let validation = generated::validate_receipt_line(
        transaction,
        WamnUuid(command.purchase_order_id.clone()),
        Json(command.line_json.clone()),
    )
    .await
    .map_err(|source| sql_error("validate record_receipt lines", source))?;
    refuse_line_outcome(
        validation.outcome.as_deref().ok_or_else(|| {
            RecordReceiptError::internal("line validator returned a null outcome")
        })?,
        validation.id,
    )?;

    let receipt_constraints = AllowedConstraints::new(
        &["receipt_purchase_order_id_receipt_reference_key"],
        &[],
        &[],
    );
    let receipt_id = claim.receipt_id.0;
    let inserted_receipt = generated::insert_receipt(
        transaction,
        WamnUuid(receipt_id.clone()),
        command.idempotency_key.clone(),
        WamnUuid(command.purchase_order_id.clone()),
        command.receipt_reference,
        TimestampTz(command.occurred_at),
    )
    .await
    .map_err(|source| {
        let error = AccessError::from_sqlx("insert receipt", &source, receipt_constraints);
        if error.kind() == AccessErrorKind::UniqueViolation
            && error.constraint() == Some("receipt_purchase_order_id_receipt_reference_key")
        {
            RecordReceiptError::with_constraint_source(
                RecordReceiptErrorKind::ReceiptReferenceConflict,
                "receipt_reference already exists for purchase_order",
                "receipt_purchase_order_id_receipt_reference_key",
                source,
            )
        } else {
            RecordReceiptError::from_classified_sqlx(error.kind(), "insert receipt", source)
        }
    })?;
    if inserted_receipt.id.0 != receipt_id {
        return Err(RecordReceiptError::internal(
            "inserted receipt id differs from the command claim",
        ));
    }

    let inserted = generated::insert_receipt_line(
        transaction,
        WamnUuid(receipt_id.clone()),
        Json(command.line_json.clone()),
    )
    .await
    .map_err(|source| sql_error("insert receipt_line", source))?;
    require_distinct_ids(
        "insert receipt_line",
        inserted.iter().map(|row| row.id.0.as_str()),
        command.line_count,
    )?;

    let updated = generated::update_purchase_order_line(
        transaction,
        WamnUuid(command.purchase_order_id.clone()),
        Json(command.line_json),
    )
    .await
    .map_err(|source| sql_error("update purchase_order_line", source))?;
    require_distinct_ids(
        "update purchase_order_line",
        updated.iter().map(|row| row.id.0.as_str()),
        command.line_count,
    )?;

    let finished =
        generated::finish_purchase_order(transaction, WamnUuid(command.purchase_order_id.clone()))
            .await
            .map_err(|source| sql_error("finish purchase_order", source))?;
    let status = PurchaseOrderStatus::parse(&finished.status)?;
    let finalized = generated::finalize_command(
        transaction,
        command.idempotency_key,
        command.canonical_command,
        WamnUuid(receipt_id.clone()),
        status.as_str().to_owned(),
        finished.row_version,
    )
    .await
    .map_err(|source| sql_error("finalize record_receipt result", source))?;
    if finalized.purchase_order_status.as_deref() != Some(status.as_str())
        || finalized.row_version != Some(finished.row_version)
    {
        return Err(RecordReceiptError::internal(
            "command ledger did not preserve the committed result",
        ));
    }
    Ok(RecordReceiptResult {
        receipt_id: receipt_id.into_boxed_str(),
        purchase_order_id: command.purchase_order_id.into_boxed_str(),
        purchase_order_status: status,
        row_version: finished.row_version,
    })
}

fn replay_result(
    replay: generated::FindReplayRow,
    canonical_command: &[u8],
) -> Result<RecordReceiptResult, RecordReceiptError> {
    if replay.canonical_command != canonical_command {
        return Err(RecordReceiptError::domain(
            RecordReceiptErrorKind::IdempotencyConflict,
            "idempotency_key is already bound to a different canonical command",
            "value.idempotency_key",
        ));
    }
    let status = replay
        .purchase_order_status
        .as_deref()
        .ok_or_else(|| RecordReceiptError::internal("replay is missing purchase_order_status"))
        .and_then(PurchaseOrderStatus::parse)?;
    let row_version = replay
        .row_version
        .ok_or_else(|| RecordReceiptError::internal("replay is missing row_version"))?;
    Ok(RecordReceiptResult {
        receipt_id: replay.receipt_id.0.into_boxed_str(),
        purchase_order_id: replay.purchase_order_id.0.into_boxed_str(),
        purchase_order_status: status,
        row_version,
    })
}

fn refuse_line_outcome(outcome: &str, id: Option<WamnUuid>) -> Result<(), RecordReceiptError> {
    let (kind, field) = match outcome {
        "ready" => return Ok(()),
        "purchase_order_line_not_found" => (
            RecordReceiptErrorKind::PurchaseOrderLineNotFound,
            "value.line[].purchase_order_line_id",
        ),
        "purchase_order_line_mismatch" => (
            RecordReceiptErrorKind::PurchaseOrderLineMismatch,
            "value.line[].purchase_order_line_id",
        ),
        "location_not_found" => (
            RecordReceiptErrorKind::LocationNotFound,
            "value.line[].location_id",
        ),
        "quantity_exceeds_remaining" => (
            RecordReceiptErrorKind::QuantityExceedsRemaining,
            "value.line[].quantity",
        ),
        _ => {
            return Err(RecordReceiptError::internal(
                "line validator returned an undeclared outcome",
            ));
        }
    };
    let id = id.ok_or_else(|| {
        RecordReceiptError::internal("line validator refusal omitted its offending id")
    })?;
    Err(RecordReceiptError::domain_id(kind, outcome, field, id.0))
}

fn sql_error(context: &'static str, source: SqlxError) -> RecordReceiptError {
    RecordReceiptError::from_sqlx(context, source, AllowedConstraints::NONE)
}

fn require_distinct_ids<'a>(
    operation: &str,
    ids: impl IntoIterator<Item = &'a str>,
    expected: usize,
) -> Result<(), RecordReceiptError> {
    let ids = ids.into_iter().collect::<BTreeSet<_>>();
    let actual = ids.len();
    if actual == expected {
        Ok(())
    } else {
        Err(RecordReceiptError::internal(format!(
            "{operation} affected {actual} rows; expected {expected}"
        )))
    }
}

fn validate_count(
    object: &str,
    field: &'static str,
    actual: usize,
    maximum: usize,
) -> Result<(), RecordReceiptError> {
    if (1..=maximum).contains(&actual) {
        Ok(())
    } else {
        Err(RecordReceiptError::invalid_range(
            format!("{object} count must be 1..={maximum}; observed {actual}"),
            field,
            1,
            maximum,
            actual,
        ))
    }
}

fn canonical_uuid(value: &str, field: &'static str) -> Result<Uuid, RecordReceiptError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|parsed| parsed.hyphenated().to_string() == value)
        .ok_or_else(|| {
            RecordReceiptError::invalid(
                format!("{field} must be a canonical lowercase UUID"),
                field,
            )
        })
}

fn canonical_timestamp(value: &str) -> Result<String, RecordReceiptError> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.to_utc())
        .filter(|timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Micros, true) == value)
        .map(|timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Micros, true))
        .ok_or_else(|| {
            RecordReceiptError::invalid(
                "value.occurred_at must be UTC RFC3339 with six fractional digits",
                "value.occurred_at",
            )
        })
}

fn validate_positive_numeric(value: &str) -> Result<(), RecordReceiptError> {
    let (integer, fraction) = match value.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (value, None),
    };
    let integer_is_canonical = !integer.is_empty()
        && integer.bytes().all(|byte| byte.is_ascii_digit())
        && (integer.len() == 1 || !integer.starts_with('0'));
    let fraction_is_canonical = fraction.is_none_or(|fraction| {
        !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit())
    });
    let positive = integer
        .bytes()
        .chain(fraction.unwrap_or_default().bytes())
        .any(|byte| byte != b'0');
    if integer_is_canonical && fraction_is_canonical && positive {
        Ok(())
    } else {
        Err(RecordReceiptError::invalid(
            "value.line[].quantity must be a positive canonical PostgreSQL numeric string",
            "value.line[].quantity",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUEST_ID: &str = "00000000-0000-0000-0000-000000000001";
    const PURCHASE_ORDER_ID: &str = "00000000-0000-0000-0000-000000000002";
    const FIRST_LINE_ID: &str = "00000000-0000-0000-0000-000000000003";
    const SECOND_LINE_ID: &str = "00000000-0000-0000-0000-000000000004";
    const LOCATION_ID: &str = "00000000-0000-0000-0000-000000000005";

    fn command(lines: Vec<RecordReceiptLine>) -> RecordReceiptInput {
        RecordReceiptInput {
            request_id: REQUEST_ID.into(),
            value: RecordReceiptValue {
                idempotency_key: "key-1".into(),
                purchase_order_id: PURCHASE_ORDER_ID.into(),
                receipt_reference: "receipt-1".into(),
                occurred_at: "2026-08-29T12:34:56.000000Z".into(),
                line: lines.into_boxed_slice(),
            },
        }
    }

    fn line(id: &str, quantity: &str) -> RecordReceiptLine {
        RecordReceiptLine {
            purchase_order_line_id: id.into(),
            quantity: quantity.into(),
            location_id: LOCATION_ID.into(),
        }
    }

    #[test]
    fn canonical_command_treats_line_order_as_presentation() {
        let first = prepare(&command(vec![
            line(SECOND_LINE_ID, "12.3400"),
            line(FIRST_LINE_ID, "1.000000"),
        ]))
        .unwrap();
        let reordered = prepare(&command(vec![
            line(FIRST_LINE_ID, "1.000000"),
            line(SECOND_LINE_ID, "12.3400"),
        ]))
        .unwrap();
        assert_eq!(first.canonical_command, reordered.canonical_command);
        assert_eq!(first.line_json, reordered.line_json);
    }

    #[test]
    fn lexical_scale_remains_command_identity() {
        let scaled = prepare(&command(vec![line(FIRST_LINE_ID, "12.3400")])).unwrap();
        let respelled = prepare(&command(vec![line(FIRST_LINE_ID, "12.34")])).unwrap();
        assert_ne!(scaled.canonical_command, respelled.canonical_command);
    }

    #[test]
    fn arbitrary_request_id_is_preserved_in_each_outcome() {
        const REQUEST_ID: &str = "submit receipt / café 🧾";
        let mut input = command(vec![line(FIRST_LINE_ID, "1.0")]);
        input.request_id = REQUEST_ID.into();
        let prepared = prepare(&input).unwrap();
        let result = RecordReceiptResult {
            receipt_id: "00000000-0000-0000-0000-000000000006".into(),
            purchase_order_id: prepared.purchase_order_id.into_boxed_str(),
            purchase_order_status: PurchaseOrderStatus::Open,
            row_version: 1,
        };

        match with_request_id(&input.request_id, Ok(result)) {
            RecordReceiptItemOutcome::Succeeded { request_id, .. } => {
                assert_eq!(request_id.as_ref(), REQUEST_ID);
            }
            RecordReceiptItemOutcome::Refused { .. } => panic!("expected success"),
        }
        match with_request_id(
            &input.request_id,
            Err(RecordReceiptError::invalid("refused for proof", "input")),
        ) {
            RecordReceiptItemOutcome::Refused { request_id, .. } => {
                assert_eq!(request_id.as_ref(), REQUEST_ID);
            }
            RecordReceiptItemOutcome::Succeeded { .. } => panic!("expected refusal"),
        }
    }

    #[test]
    fn database_source_is_retained_but_not_exposed_by_the_operation_error() {
        let error = RecordReceiptError::from(SqlxError::Protocol(
            "private database diagnostic".to_owned(),
        ));

        assert_eq!(error.kind(), RecordReceiptErrorKind::InternalError);
        assert!(error.source().is_some());
        assert!(!error.to_string().contains("private database diagnostic"));
    }

    #[test]
    fn malformed_shapes_refuse_with_invalid_input() {
        for actual in [0, MAX_RECORD_RECEIPT_ITEMS + 1] {
            assert_eq!(
                validate_count(
                    "record_receipt item",
                    "input",
                    actual,
                    MAX_RECORD_RECEIPT_ITEMS,
                )
                .unwrap_err()
                .kind(),
                RecordReceiptErrorKind::InvalidInput
            );
        }
        for actual in [0, MAX_RECORD_RECEIPT_LINES + 1] {
            assert_eq!(
                validate_count(
                    "record_receipt line",
                    "value.line",
                    actual,
                    MAX_RECORD_RECEIPT_LINES,
                )
                .unwrap_err()
                .kind(),
                RecordReceiptErrorKind::InvalidInput
            );
        }
        let duplicate = command(vec![line(FIRST_LINE_ID, "1.0"), line(FIRST_LINE_ID, "2.0")]);
        assert_eq!(
            prepare(&duplicate).unwrap_err().kind(),
            RecordReceiptErrorKind::InvalidInput
        );
        for quantity in ["0", "0.0000", "01.0", "-1", "1.", ".1", "1e2"] {
            assert_eq!(
                prepare(&command(vec![line(FIRST_LINE_ID, quantity)]))
                    .unwrap_err()
                    .kind(),
                RecordReceiptErrorKind::InvalidInput
            );
        }
        assert_eq!(
            prepare(&command(Vec::new())).unwrap_err().kind(),
            RecordReceiptErrorKind::InvalidInput
        );
    }

    #[test]
    fn all_closed_literals_are_distinct() {
        let kinds = [
            RecordReceiptErrorKind::InvalidInput,
            RecordReceiptErrorKind::PurchaseOrderNotFound,
            RecordReceiptErrorKind::PurchaseOrderNotOpen,
            RecordReceiptErrorKind::PurchaseOrderLineNotFound,
            RecordReceiptErrorKind::PurchaseOrderLineMismatch,
            RecordReceiptErrorKind::LocationNotFound,
            RecordReceiptErrorKind::QuantityExceedsRemaining,
            RecordReceiptErrorKind::ReceiptReferenceConflict,
            RecordReceiptErrorKind::IdempotencyConflict,
            RecordReceiptErrorKind::Retry,
            RecordReceiptErrorKind::Timeout,
            RecordReceiptErrorKind::PermissionDenied,
            RecordReceiptErrorKind::InternalError,
        ];
        assert_eq!(
            kinds
                .iter()
                .map(|kind| kind.literal())
                .collect::<BTreeSet<_>>()
                .len(),
            kinds.len()
        );
    }
}
