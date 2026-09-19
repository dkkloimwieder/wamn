//! Wire adapters for Acme Receiving operations backed by generated SQL.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wamn_postgres_statements::{Connection, Uuid as WamnUuid};

use crate::error::{AccessError, AccessErrorKind, AllowedConstraints};
use crate::generated::{
    purchase_order as purchase_order_sql, quality_approve_inspection as approve_sql,
    quality_create_inspection as create_sql, quality_load_purchase_order_detail as detail_sql,
};

pub use crate::generated::purchase_order::PurchaseOrderRow;

const MAX_ENVELOPE_ITEMS: usize = 100;
const UPDATE_CONSTRAINTS: AllowedConstraints = AllowedConstraints {
    exclusion: purchase_order_sql::UPDATE_EXCLUSION_CONSTRAINTS,
};

/// Envelope refusal translated to the frozen node invalid-input arm.
#[derive(Debug)]
pub struct InvocationError {
    context: &'static str,
}

impl InvocationError {
    /// Frozen WIT error-detail code.
    pub const fn code(&self) -> &'static str {
        "invalid_input"
    }

    /// Stable, non-database refusal description.
    pub const fn context(&self) -> &'static str {
        self.context
    }

    const fn new(context: &'static str) -> Self {
        Self { context }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct EnvelopeItem {
    request_id: Box<str>,
    body: Value,
}

fn prepare_envelope(input: &str) -> Result<Box<[EnvelopeItem]>, InvocationError> {
    let value: Value = serde_json::from_str(input)
        .map_err(|_| InvocationError::new("operation input must be a JSON array"))?;
    let Value::Array(values) = value else {
        return Err(InvocationError::new("operation input must be a JSON array"));
    };
    if !(1..=MAX_ENVELOPE_ITEMS).contains(&values.len()) {
        return Err(InvocationError::new(
            "operation input item count must be 1..=100",
        ));
    }

    values
        .into_iter()
        .map(|value| {
            let Value::Object(mut object) = value else {
                return Err(InvocationError::new(
                    "every operation item must be a JSON object",
                ));
            };
            let request_id = match object.remove("request_id") {
                Some(Value::String(request_id)) if !request_id.is_empty() => request_id,
                _ => {
                    return Err(InvocationError::new(
                        "every operation item must carry a nonempty string request_id",
                    ));
                }
            };
            Ok(EnvelopeItem {
                request_id: request_id.into_boxed_str(),
                body: Value::Object(object),
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Vec::into_boxed_slice)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetInput {
    id: Box<str>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoadPurchaseOrderDetailInput {
    purchase_order_id: Box<str>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveInspectionInput {
    receipt_id: Box<str>,
    expected_row_version: Box<str>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateInspectionInput {
    event: InsertEvent,
    new: NewReceipt,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum InsertEvent {
    Insert,
}

#[derive(Debug, Deserialize)]
struct NewReceipt {
    id: Box<str>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ItemResult<T> {
    Succeeded {
        request_id: Box<str>,
        value: T,
    },
    Refused {
        request_id: Box<str>,
        error: OperationError,
    },
}

#[derive(Debug, Serialize)]
struct OperationError {
    code: &'static str,
    detail: ErrorDetail,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ErrorDetail {
    Field(FieldDetail),
    FieldId(FieldIdDetail),
    Concurrency(ConcurrencyDetail),
    Permission(PermissionDetail),
    Constraint(ConstraintDetail),
    Empty(EmptyDetail),
}

#[derive(Debug, Serialize)]
struct FieldDetail {
    field: &'static str,
}

#[derive(Debug, Serialize)]
struct FieldIdDetail {
    field: &'static str,
    id: Box<str>,
}

#[derive(Debug, Serialize)]
struct ConcurrencyDetail {
    expected_row_version: Box<str>,
    observed_row_version: Box<str>,
}

#[derive(Debug, Serialize)]
struct PermissionDetail {
    operation: &'static str,
}

#[derive(Debug, Serialize)]
struct ConstraintDetail {
    constraint: Box<str>,
}

#[derive(Debug, Serialize)]
struct EmptyDetail {}

#[derive(Debug, Serialize)]
struct PurchaseOrderValue {
    id: Box<str>,
    purchase_order_number: Box<str>,
    supplier_id: Box<str>,
    status: Box<str>,
    row_version: Box<str>,
    created_at: Box<str>,
    created_by: Box<str>,
    updated_at: Box<str>,
    updated_by: Box<str>,
    acme_inspection_required: bool,
    acme_quality_status: Box<str>,
}

impl From<purchase_order_sql::PurchaseOrderRow> for PurchaseOrderValue {
    fn from(row: purchase_order_sql::PurchaseOrderRow) -> Self {
        Self {
            id: row.id.0.into_boxed_str(),
            purchase_order_number: row.purchase_order_number.into_boxed_str(),
            supplier_id: row.supplier_id.0.into_boxed_str(),
            status: row.status.into_boxed_str(),
            row_version: row.row_version.to_string().into_boxed_str(),
            created_at: row.created_at.0.into_boxed_str(),
            created_by: row.created_by.0.into_boxed_str(),
            updated_at: row.updated_at.0.into_boxed_str(),
            updated_by: row.updated_by.0.into_boxed_str(),
            acme_inspection_required: row.acme_inspection_required,
            acme_quality_status: row.acme_quality_status.into_boxed_str(),
        }
    }
}

#[derive(Debug, Serialize)]
struct PurchaseOrderDetailValue {
    id: Box<str>,
    purchase_order_number: Box<str>,
    supplier_id: Box<str>,
    status: Box<str>,
    row_version: Box<str>,
    acme_inspection_required: bool,
    acme_quality_status: Box<str>,
}

impl From<detail_sql::LoadPurchaseOrderDetailRow> for PurchaseOrderDetailValue {
    fn from(row: detail_sql::LoadPurchaseOrderDetailRow) -> Self {
        Self {
            id: row.id.0.into_boxed_str(),
            purchase_order_number: row.purchase_order_number.into_boxed_str(),
            supplier_id: row.supplier_id.0.into_boxed_str(),
            status: row.status.into_boxed_str(),
            row_version: row.row_version.to_string().into_boxed_str(),
            acme_inspection_required: row.acme_inspection_required,
            acme_quality_status: row.acme_quality_status.into_boxed_str(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ApproveInspectionValue {
    receipt_id: Box<str>,
    status: Box<str>,
    row_version: Box<str>,
    purchase_order_id: Box<str>,
    purchase_order_row_version: Box<str>,
}

fn parse_item<T: DeserializeOwned>(item: &EnvelopeItem) -> Result<T, OperationError> {
    serde_json::from_value(item.body.clone()).map_err(|_| invalid_input("input"))
}

/// Parse any accepted UUID spelling and re-spell it lowercase-hyphenated.
///
/// Case is representation, so an input arrives in any spelling and reaches the
/// database in one. The idempotency key hashes the re-spelled bytes, never the
/// arriving bytes, so a caller whose retry infrastructure re-spells a UUID gets
/// one command instead of an idempotency conflict.
fn parse_uuid(value: &str, field: &'static str) -> Result<WamnUuid, AccessError> {
    uuid::Uuid::parse_str(value)
        .map(|parsed| WamnUuid(parsed.hyphenated().to_string()))
        .map_err(|_| AccessError::invalid("input is not a UUID", field))
}

/// Parse any accepted int64 spelling and re-spell it as the canonical decimal.
///
/// Numerics stay strict on scale, because scale is value in PostgreSQL, where
/// `12.3400` is not `12.34`; integers and UUIDs re-spell because their text is
/// representation only. Same law, two facts about types.
fn parse_int64(value: &str, field: &'static str) -> Result<i64, AccessError> {
    value
        .parse::<i64>()
        .map_err(|_| AccessError::invalid("input is not an int64", field))
}

#[expect(
    clippy::option_option,
    reason = "WIT update fields distinguish absent, explicit null, and value"
)]
fn update_value<T>(
    value: Option<Option<T>>,
    field: &'static str,
) -> Result<(bool, Option<T>), AccessError> {
    match value {
        None => Ok((false, None)),
        Some(None) => Err(AccessError::invalid(
            "non-null field does not accept explicit null",
            field,
        )),
        Some(Some(value)) => Ok((true, Some(value))),
    }
}

fn quality_status(value: String) -> Result<String, AccessError> {
    if matches!(value.as_str(), "not_required" | "pending" | "approved") {
        Ok(value)
    } else {
        Err(AccessError::invalid(
            "acme_quality_status is outside the closed vocabulary",
            "change.acme_quality_status",
        ))
    }
}

fn invalid_input(field: &'static str) -> OperationError {
    OperationError {
        code: "invalid_input",
        detail: ErrorDetail::Field(FieldDetail { field }),
    }
}

fn access_error(
    error: &AccessError,
    operation: &'static str,
    not_found: Option<(&'static str, &str)>,
    expected_row_version: Option<i64>,
) -> OperationError {
    let empty = || ErrorDetail::Empty(EmptyDetail {});
    let internal = || OperationError {
        code: "internal_error",
        detail: empty(),
    };
    let detail = match error.kind() {
        AccessErrorKind::InvalidInput => {
            let Some(field) = error.field() else {
                return internal();
            };
            ErrorDetail::Field(FieldDetail { field })
        }
        AccessErrorKind::NotFound => {
            let Some((field, id)) = not_found else {
                return internal();
            };
            ErrorDetail::FieldId(FieldIdDetail {
                field,
                id: id.into(),
            })
        }
        AccessErrorKind::ConcurrencyConflict => {
            let (Some(expected), Some(observed)) =
                (expected_row_version, error.observed_row_version())
            else {
                return internal();
            };
            ErrorDetail::Concurrency(ConcurrencyDetail {
                expected_row_version: expected.to_string().into_boxed_str(),
                observed_row_version: observed.to_string().into_boxed_str(),
            })
        }
        AccessErrorKind::ExclusionViolation => {
            let Some(constraint) = error.constraint() else {
                return internal();
            };
            ErrorDetail::Constraint(ConstraintDetail {
                constraint: constraint.into(),
            })
        }
        AccessErrorKind::PermissionDenied => {
            ErrorDetail::Permission(PermissionDetail { operation })
        }
        AccessErrorKind::Retry | AccessErrorKind::Timeout | AccessErrorKind::InternalError => {
            empty()
        }
    };
    OperationError {
        code: error.kind().literal(),
        detail,
    }
}

fn refused<T>(request_id: Box<str>, error: OperationError) -> ItemResult<T> {
    ItemResult::Refused { request_id, error }
}

fn serialized<T: Serialize>(output: &[ItemResult<T>]) -> String {
    serde_json::to_string(output).expect("closed operation results always serialize")
}

fn purchase_order_update_value(
    row: purchase_order_sql::PurchaseOrderUpdateRow,
    expected_row_version: i64,
) -> Result<PurchaseOrderRow, AccessError> {
    match row.outcome.as_deref() {
        Some("not_found") => Err(AccessError::not_found("purchase_order does not exist")),
        Some("concurrency_conflict") => row.observed_row_version.map_or_else(
            || {
                Err(AccessError::internal(
                    "purchase_order concurrency refusal omitted observed_row_version",
                ))
            },
            |observed| {
                Err(AccessError::concurrency_conflict(
                    format!(
                        "purchase_order row_version {observed} does not match {expected_row_version}"
                    ),
                    observed,
                ))
            },
        ),
        Some("updated") => match (
            row.id,
            row.purchase_order_number,
            row.supplier_id,
            row.status,
            row.row_version,
            row.created_at,
            row.created_by,
            row.updated_at,
            row.updated_by,
            row.acme_inspection_required,
            row.acme_quality_status,
        ) {
            (
                Some(id),
                Some(purchase_order_number),
                Some(supplier_id),
                Some(status),
                Some(row_version),
                Some(created_at),
                Some(created_by),
                Some(updated_at),
                Some(updated_by),
                Some(acme_inspection_required),
                Some(acme_quality_status),
            ) => Ok(PurchaseOrderRow {
                id,
                purchase_order_number,
                supplier_id,
                status,
                row_version,
                created_at,
                created_by,
                updated_at,
                updated_by,
                acme_inspection_required,
                acme_quality_status,
            }),
            _ => Err(AccessError::internal(
                "purchase_order update returned an incomplete row",
            )),
        },
        _ => Err(AccessError::internal(
            "purchase_order.update returned a null or unknown outcome",
        )),
    }
}

fn approve_inspection_value(
    row: approve_sql::ApproveInspectionRow,
    expected_row_version: i64,
) -> Result<ApproveInspectionValue, AccessError> {
    match row.outcome.as_deref() {
        Some("not_found") => Err(AccessError::not_found("quality_inspection does not exist")),
        Some("concurrency_conflict") => row.observed_row_version.map_or_else(
            || {
                Err(AccessError::internal(
                    "quality_inspection concurrency refusal omitted observed_row_version",
                ))
            },
            |observed| {
                Err(AccessError::concurrency_conflict(
                    format!(
                        "quality_inspection row_version {observed} does not match {expected_row_version}"
                    ),
                    observed,
                ))
            },
        ),
        Some("approved") => match (
            row.receipt_id,
            row.status,
            row.row_version,
            row.purchase_order_id,
            row.purchase_order_row_version,
        ) {
            (
                Some(receipt_id),
                Some(status),
                Some(row_version),
                Some(purchase_order_id),
                Some(purchase_order_row_version),
            ) if status == "approved" => Ok(ApproveInspectionValue {
                receipt_id: receipt_id.0.into_boxed_str(),
                status: status.into_boxed_str(),
                row_version: row_version.to_string().into_boxed_str(),
                purchase_order_id: purchase_order_id.0.into_boxed_str(),
                purchase_order_row_version: purchase_order_row_version
                    .to_string()
                    .into_boxed_str(),
            }),
            _ => Err(AccessError::internal(
                "quality_inspection approval returned an incomplete row",
            )),
        },
        _ => Err(AccessError::internal(
            "quality.approve_inspection returned a null or unknown outcome",
        )),
    }
}

/// Execute `purchase_order.get` against generated Acme SQL.
pub async fn purchase_order_get(input: &str) -> Result<String, InvocationError> {
    let items = prepare_envelope(input)?;
    let mut connection = Connection::new();
    let mut output: Vec<ItemResult<PurchaseOrderValue>> = Vec::with_capacity(items.len());
    for item in items.into_vec() {
        let parsed = match parse_item::<GetInput>(&item) {
            Ok(parsed) => parsed,
            Err(error) => {
                output.push(refused(item.request_id, error));
                continue;
            }
        };
        let result = match parse_uuid(&parsed.id, "id") {
            Ok(id) => purchase_order_sql::get(&mut connection, id)
                .await
                .map_err(|source| AccessError::from_statement("load purchase_order", &source))
                .and_then(|row| {
                    row.ok_or_else(|| AccessError::not_found("purchase_order does not exist"))
                }),
            Err(error) => Err(error),
        };
        output.push(match result {
            Ok(value) => ItemResult::Succeeded {
                request_id: item.request_id,
                value: value.into(),
            },
            Err(error) => refused(
                item.request_id,
                access_error(&error, "purchase_order.get", Some(("id", &parsed.id)), None),
            ),
        });
    }
    Ok(serialized(&output))
}

/// Execute one typed `purchase_order.update` against generated Acme SQL.
#[expect(
    clippy::option_option,
    reason = "WIT update fields distinguish absent, explicit null, and value"
)]
pub async fn purchase_order_update(
    connection: &mut Connection,
    id: &str,
    expected_row_version: i64,
    acme_inspection_required: Option<Option<bool>>,
    acme_quality_status: Option<Option<String>>,
) -> Result<PurchaseOrderRow, AccessError> {
    let id = parse_uuid(id, "id")?;
    let (inspection_present, inspection_value) =
        update_value(acme_inspection_required, "change.acme_inspection_required")?;
    let (quality_present, quality_value) =
        update_value(acme_quality_status, "change.acme_quality_status")?;
    let quality_value = quality_value.map(quality_status).transpose()?;
    let row = purchase_order_sql::update(
        connection,
        id,
        expected_row_version,
        inspection_present,
        inspection_value,
        quality_present,
        quality_value,
    )
    .await
    .map_err(|source| {
        AccessError::from_statement_with_constraints(
            "update purchase_order",
            &source,
            UPDATE_CONSTRAINTS,
        )
    })?;
    purchase_order_update_value(row, expected_row_version)
}

/// Execute `quality.load_purchase_order_detail` against its verified projection.
pub async fn quality_load_purchase_order_detail(input: &str) -> Result<String, InvocationError> {
    let items = prepare_envelope(input)?;
    let mut connection = Connection::new();
    let mut output: Vec<ItemResult<PurchaseOrderDetailValue>> = Vec::with_capacity(items.len());
    for item in items.into_vec() {
        let parsed = match parse_item::<LoadPurchaseOrderDetailInput>(&item) {
            Ok(parsed) => parsed,
            Err(error) => {
                output.push(refused(item.request_id, error));
                continue;
            }
        };
        let result = async {
            let id = parse_uuid(&parsed.purchase_order_id, "purchase_order_id")?;
            let mut transaction = connection.begin().await.map_err(|source| {
                AccessError::from_statement("begin purchase_order detail", &source)
            })?;
            let row = detail_sql::load_purchase_order_detail(&mut transaction, id)
                .await
                .map_err(|source| {
                    AccessError::from_statement("load purchase_order detail", &source)
                })?;
            transaction.commit().await.map_err(|source| {
                AccessError::from_statement("commit purchase_order detail", &source)
            })?;
            row.map(Into::into)
                .ok_or_else(|| AccessError::not_found("purchase_order does not exist"))
        }
        .await;
        output.push(match result {
            Ok(value) => ItemResult::Succeeded {
                request_id: item.request_id,
                value,
            },
            Err(error) => refused(
                item.request_id,
                access_error(
                    &error,
                    "quality.load_purchase_order_detail",
                    Some(("purchase_order_id", &parsed.purchase_order_id)),
                    None,
                ),
            ),
        });
    }
    Ok(serialized(&output))
}

/// Execute `quality.approve_inspection` as one transaction per input item.
pub async fn quality_approve_inspection(input: &str) -> Result<String, InvocationError> {
    let items = prepare_envelope(input)?;
    let mut connection = Connection::new();
    let mut output: Vec<ItemResult<ApproveInspectionValue>> = Vec::with_capacity(items.len());
    for item in items.into_vec() {
        let parsed = match parse_item::<ApproveInspectionInput>(&item) {
            Ok(parsed) => parsed,
            Err(error) => {
                output.push(refused(item.request_id, error));
                continue;
            }
        };
        let result = async {
            let receipt_id = parse_uuid(&parsed.receipt_id, "receipt_id")?;
            let expected_row_version =
                parse_int64(&parsed.expected_row_version, "expected_row_version")?;
            let mut transaction = connection.begin().await.map_err(|source| {
                AccessError::from_statement("begin inspection approval", &source)
            })?;
            let row =
                approve_sql::approve_inspection(&mut transaction, receipt_id, expected_row_version)
                    .await
                    .map_err(|source| {
                        AccessError::from_statement("approve quality_inspection", &source)
                    })?;
            let value = approve_inspection_value(row, expected_row_version)?;
            transaction.commit().await.map_err(|source| {
                AccessError::from_statement("commit inspection approval", &source)
            })?;
            Ok::<_, AccessError>(value)
        }
        .await;
        let expected = parse_int64(&parsed.expected_row_version, "expected_row_version").ok();
        output.push(match result {
            Ok(value) => ItemResult::Succeeded {
                request_id: item.request_id,
                value,
            },
            Err(error) => refused(
                item.request_id,
                access_error(
                    &error,
                    "quality.approve_inspection",
                    Some(("receipt_id", &parsed.receipt_id)),
                    expected,
                ),
            ),
        });
    }
    Ok(serialized(&output))
}

/// Execute private `quality.create_inspection` without caller or permission synthesis.
pub async fn quality_create_inspection(input: &str) -> Result<String, AccessError> {
    let parsed: CreateInspectionInput = serde_json::from_str(input)
        .map_err(|_| AccessError::invalid("event input does not match its contract", "input"))?;
    let InsertEvent::Insert = parsed.event;
    let receipt_id = parse_uuid(&parsed.new.id, "new.id")?;
    let mut connection = Connection::new();
    let mut transaction = connection
        .begin()
        .await
        .map_err(|source| AccessError::from_statement("begin inspection creation", &source))?;
    let inserted = create_sql::insert_inspection(&mut transaction, receipt_id.clone())
        .await
        .map_err(|source| AccessError::from_statement("insert quality_inspection", &source))?;
    let persisted_id = match inserted {
        Some(row) => row.receipt_id,
        None => {
            create_sql::load_inspection(&mut transaction, receipt_id.clone())
                .await
                .map_err(|source| {
                    AccessError::from_statement("load quality_inspection replay", &source)
                })?
                .ok_or_else(|| {
                    AccessError::internal(
                        "quality_inspection conflict did not resolve to the receipt id",
                    )
                })?
                .receipt_id
        }
    };
    if persisted_id != receipt_id {
        return Err(AccessError::internal(
            "quality_inspection returned a different receipt id",
        ));
    }
    transaction
        .commit()
        .await
        .map_err(|source| AccessError::from_statement("commit inspection creation", &source))?;
    Ok(input.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    fn exclusion_refusal(constraint: Option<&str>, allowed: AllowedConstraints) -> String {
        let error = AccessError::from_statement_parts(
            "update purchase_order",
            wamn_postgres_statements::StatementErrorKind::ExclusionViolation,
            constraint,
            allowed,
        );
        let result: ItemResult<()> = refused(
            "exclusion-test".into(),
            access_error(&error, "purchase_order.update", None, Some(1)),
        );
        serialized(&[result])
    }

    #[test]
    fn exclusion_refusal_retains_only_allowed_constraint_detail() {
        let allowed = AllowedConstraints {
            exclusion: &["purchase_order_allowed_exclusion"],
        };
        assert_eq!(
            exclusion_refusal(Some("purchase_order_allowed_exclusion"), allowed),
            r#"[{"request_id":"exclusion-test","error":{"code":"exclusion_violation","detail":{"constraint":"purchase_order_allowed_exclusion"}}}]"#
        );
        for (constraint, allowed) in [
            (None, allowed),
            (Some("unlisted_exclusion"), allowed),
            (
                Some("purchase_order_allowed_exclusion"),
                AllowedConstraints::NONE,
            ),
        ] {
            assert_eq!(
                exclusion_refusal(constraint, allowed),
                r#"[{"request_id":"exclusion-test","error":{"code":"internal_error","detail":{}}}]"#
            );
        }
    }

    // The exclusion_diagnostics fixture regenerates this package with test-only exclusions.
    #[test]
    #[ignore = "requires: WAMN_EXCLUSION_DIAGNOSTICS"]
    fn generated_update_exclusion_from_postgres() {
        let path = std::env::var_os("WAMN_EXCLUSION_DIAGNOSTICS")
            .expect("WAMN_EXCLUSION_DIAGNOSTICS must name the disposable PostgreSQL diagnostics");
        let diagnostics: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let diagnostic = &diagnostics["client_acme_receiving"];
        assert_eq!(diagnostic["sqlstate"], "23P01");
        let constraint = diagnostic["constraint"]
            .as_str()
            .expect("server constraint name");
        assert_eq!(
            exclusion_refusal(Some(constraint), UPDATE_CONSTRAINTS),
            r#"[{"request_id":"exclusion-test","error":{"code":"exclusion_violation","detail":{"constraint":"purchase_order_acme_quality_status_excl"}}}]"#
        );
    }

    #[test]
    fn envelope_requires_all_request_ids_before_item_processing() {
        let input = format!(r#"[{{"request_id":"first","id":"{ID}"}},{{"id":"{ID}"}}]"#);
        let error = prepare_envelope(&input).expect_err("a missing request_id must refuse");
        assert_eq!(error.code(), "invalid_input");
        assert_eq!(
            error.context(),
            "every operation item must carry a nonempty string request_id"
        );
    }

    #[test]
    fn non_int64_and_non_uuid_wire_scalars_refuse() {
        assert!(parse_uuid(ID, "id").is_ok());
        assert!(parse_uuid("not-a-uuid", "id").is_err());
        assert_eq!(parse_int64("-42", "row_version").unwrap(), -42);
        for value in ["", "1.0", "9223372036854775808"] {
            assert_eq!(
                parse_int64(value, "row_version").unwrap_err().kind(),
                AccessErrorKind::InvalidInput
            );
        }
    }

    /// This overlay mints no idempotency key of its own; its command identity is
    /// the canonical parameter text ingest hands to SQL, which the base
    /// `record_receipt` key hashes. Two case-spellings of one UUID must
    /// therefore arrive as one command rather than as an `idempotency_conflict`.
    #[test]
    fn two_spellings_of_one_uuid_make_one_command() {
        let send = |receipt_id: &str| {
            let parsed: ApproveInspectionInput = serde_json::from_value(serde_json::json!({
                "receipt_id": receipt_id,
                "expected_row_version": "1",
            }))
            .expect("the approve_inspection contract accepts either spelling");
            parse_uuid(&parsed.receipt_id, "receipt_id")
                .expect("either spelling of one UUID is a UUID")
        };
        let first = send(ID);
        let second = send(&ID.to_uppercase());
        assert_eq!(first, second);
        assert_eq!(second.0, ID);
    }

    /// Same boundary, same law, on the other type this overlay ingests. Two
    /// spellings of one integer must arrive as one command rather than as an
    /// `idempotency_conflict`.
    #[test]
    fn two_spellings_of_one_int64_make_one_command() {
        let send = |expected_row_version: &str| {
            let parsed: ApproveInspectionInput = serde_json::from_value(serde_json::json!({
                "receipt_id": ID,
                "expected_row_version": expected_row_version,
            }))
            .expect("the approve_inspection contract accepts either spelling");
            parse_int64(&parsed.expected_row_version, "expected_row_version")
                .expect("either spelling of one integer is an int64")
        };
        assert_eq!(send("01"), send("1"));
        assert_eq!(send("+1"), 1);
    }

    #[test]
    fn private_event_requires_insert_and_receipt_id_without_caller_fields() {
        let parsed: CreateInspectionInput = serde_json::from_value(serde_json::json!({
            "event": "insert",
            "new": {"id": ID, "purchase_order_id": ID}
        }))
        .unwrap();
        assert_eq!(parsed.new.id.as_ref(), ID);
        for fabricated in ["caller", "permission"] {
            let value = serde_json::json!({
                "event": "insert",
                "new": {"id": ID},
                fabricated: "fabricated"
            });
            assert!(serde_json::from_value::<CreateInspectionInput>(value).is_err());
        }
    }

    #[test]
    fn persisted_int64_and_structured_refusals_keep_closed_wire_shapes() {
        let value = PurchaseOrderValue {
            id: ID.into(),
            purchase_order_number: "PO-1".into(),
            supplier_id: ID.into(),
            status: "open".into(),
            row_version: 42_i64.to_string().into_boxed_str(),
            created_at: "2026-09-01T12:00:00Z".into(),
            created_by: ID.into(),
            updated_at: "2026-09-01T12:00:00Z".into(),
            updated_by: ID.into(),
            acme_inspection_required: true,
            acme_quality_status: "pending".into(),
        };
        assert_eq!(serde_json::to_value(value).unwrap()["row_version"], "42");

        let error = OperationError {
            code: "concurrency_conflict",
            detail: ErrorDetail::Concurrency(ConcurrencyDetail {
                expected_row_version: "4".into(),
                observed_row_version: "5".into(),
            }),
        };
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            serde_json::json!({
                "code": "concurrency_conflict",
                "detail": {"expected_row_version": "4", "observed_row_version": "5"}
            })
        );
    }
}
