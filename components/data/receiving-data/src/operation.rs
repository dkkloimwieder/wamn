//! Shared wire adapter for the six operations exported by the Receiving component.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use wamn_postgres_sqlx::WamnConnection;

use crate::error::{AccessError, AccessErrorKind};
use crate::record_receipt::{RecordReceiptError, RecordReceiptErrorKind};
use crate::{purchase_order, receipt, record_receipt};

const MAX_ENVELOPE_ITEMS: usize = 100;

/// Envelope-level refusal translated to the frozen `wamn:node` invalid-input arm.
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

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PurchaseOrderFilterInput {
    #[serde(default)]
    supplier_id: Option<Box<[Box<str>]>>,
    #[serde(default)]
    status: Option<Box<[PurchaseOrderStatusInput]>>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PurchaseOrderStatusInput {
    Open,
    Complete,
    Cancelled,
}

impl From<PurchaseOrderStatusInput> for purchase_order::PurchaseOrderStatus {
    fn from(value: PurchaseOrderStatusInput) -> Self {
        match value {
            PurchaseOrderStatusInput::Open => Self::Open,
            PurchaseOrderStatusInput::Complete => Self::Complete,
            PurchaseOrderStatusInput::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SortFieldInput {
    PurchaseOrderNumber,
    Status,
    CreatedAt,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SortDirectionInput {
    Ascending,
    Descending,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SortInput {
    field: SortFieldInput,
    direction: SortDirectionInput,
}

impl From<SortInput> for purchase_order::PurchaseOrderSort {
    fn from(value: SortInput) -> Self {
        match (value.field, value.direction) {
            (SortFieldInput::PurchaseOrderNumber, SortDirectionInput::Ascending) => {
                Self::PurchaseOrderNumberAscending
            }
            (SortFieldInput::PurchaseOrderNumber, SortDirectionInput::Descending) => {
                Self::PurchaseOrderNumberDescending
            }
            (SortFieldInput::Status, SortDirectionInput::Ascending) => Self::StatusAscending,
            (SortFieldInput::Status, SortDirectionInput::Descending) => Self::StatusDescending,
            (SortFieldInput::CreatedAt, SortDirectionInput::Ascending) => Self::CreatedAtAscending,
            (SortFieldInput::CreatedAt, SortDirectionInput::Descending) => {
                Self::CreatedAtDescending
            }
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PurchaseOrderQueryInput {
    #[serde(default)]
    filter: Option<PurchaseOrderFilterInput>,
    #[serde(default)]
    sort: Option<SortInput>,
    #[serde(default)]
    cursor: Option<Box<str>>,
    #[serde(default)]
    limit: Option<i64>,
}

impl From<PurchaseOrderQueryInput> for purchase_order::QueryInput {
    fn from(value: PurchaseOrderQueryInput) -> Self {
        let filter = value.filter.unwrap_or_default();
        Self {
            supplier_ids: filter.supplier_id,
            statuses: filter.status.map(|values| {
                values
                    .into_vec()
                    .into_iter()
                    .map(Into::into)
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            }),
            sort: value.sort.map(Into::into).unwrap_or_default(),
            cursor: value.cursor,
            limit: value.limit,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptQueryInput {
    #[serde(default)]
    cursor: Option<Box<str>>,
    #[serde(default)]
    limit: Option<i64>,
}

impl From<ReceiptQueryInput> for receipt::QueryInput {
    fn from(value: ReceiptQueryInput) -> Self {
        Self {
            cursor: value.cursor,
            limit: value.limit,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PurchaseOrderUpdateInput {
    id: Box<str>,
    expected_row_version: Box<str>,
    change: PurchaseOrderChangeInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PurchaseOrderChangeInput {
    #[serde(default, deserialize_with = "deserialize_nullable")]
    supplier_id: Nullable<Box<str>>,
}

#[derive(Debug, Default)]
enum Nullable<T> {
    #[default]
    Omitted,
    Null,
    Value(T),
}

fn deserialize_nullable<'de, D, T>(deserializer: D) -> Result<Nullable<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(|value| match value {
        Some(value) => Nullable::Value(value),
        None => Nullable::Null,
    })
}

impl From<Nullable<Box<str>>> for purchase_order::SupplierIdUpdate {
    fn from(value: Nullable<Box<str>>) -> Self {
        match value {
            Nullable::Omitted => Self::Omitted,
            Nullable::Null => Self::Null,
            Nullable::Value(value) => Self::Value(value),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordReceiptInput {
    value: RecordReceiptValue,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordReceiptValue {
    idempotency_key: Box<str>,
    purchase_order_id: Box<str>,
    receipt_reference: Box<str>,
    occurred_at: Box<str>,
    line: Box<[RecordReceiptLine]>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordReceiptLine {
    purchase_order_line_id: Box<str>,
    quantity: Box<str>,
    location_id: Box<str>,
}

fn record_receipt_input(
    request_id: Box<str>,
    value: RecordReceiptInput,
) -> record_receipt::RecordReceiptInput {
    record_receipt::RecordReceiptInput {
        request_id,
        value: record_receipt::RecordReceiptValue {
            idempotency_key: value.value.idempotency_key,
            purchase_order_id: value.value.purchase_order_id,
            receipt_reference: value.value.receipt_reference,
            occurred_at: value.value.occurred_at,
            line: value
                .value
                .line
                .into_vec()
                .into_iter()
                .map(|line| record_receipt::RecordReceiptLine {
                    purchase_order_line_id: line.purchase_order_line_id,
                    quantity: line.quantity,
                    location_id: line.location_id,
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        },
    }
}

fn parse_int64(value: &str) -> Result<i64, ()> {
    value
        .parse::<i64>()
        .ok()
        .filter(|parsed| parsed.to_string() == value)
        .ok_or(())
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
    InvalidInput(InvalidInputDetail),
    Field(FieldDetail),
    FieldId(FieldIdDetail),
    Concurrency(ConcurrencyDetail),
    Constraint(ConstraintDetail),
    Permission(PermissionDetail),
    Empty(EmptyDetail),
}

#[derive(Debug, Serialize)]
struct InvalidInputDetail {
    field: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    minimum: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    maximum: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    observed: Option<i64>,
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
struct ConstraintDetail {
    constraint: Box<str>,
}

#[derive(Debug, Serialize)]
struct PermissionDetail {
    operation: &'static str,
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
    updated_at: Box<str>,
}

impl From<purchase_order::PurchaseOrderRow> for PurchaseOrderValue {
    fn from(row: purchase_order::PurchaseOrderRow) -> Self {
        Self {
            id: row.id.0.into_boxed_str(),
            purchase_order_number: row.purchase_order_number.into_boxed_str(),
            supplier_id: row.supplier_id.0.into_boxed_str(),
            status: row.status.into_boxed_str(),
            row_version: row.row_version.to_string().into_boxed_str(),
            created_at: row.created_at.0.into_boxed_str(),
            updated_at: row.updated_at.0.into_boxed_str(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ReceiptValue {
    id: Box<str>,
    idempotency_key: Box<str>,
    purchase_order_id: Box<str>,
    receipt_reference: Box<str>,
    occurred_at: Box<str>,
    created_at: Box<str>,
}

impl From<receipt::ReceiptRow> for ReceiptValue {
    fn from(row: receipt::ReceiptRow) -> Self {
        Self {
            id: row.id.0.into_boxed_str(),
            idempotency_key: row.idempotency_key.into_boxed_str(),
            purchase_order_id: row.purchase_order_id.0.into_boxed_str(),
            receipt_reference: row.receipt_reference.into_boxed_str(),
            occurred_at: row.occurred_at.0.into_boxed_str(),
            created_at: row.created_at.0.into_boxed_str(),
        }
    }
}

#[derive(Debug, Serialize)]
struct PageValue<T> {
    item: Box<[T]>,
    next_cursor: Option<Box<str>>,
}

fn purchase_order_page(page: purchase_order::Page) -> PageValue<PurchaseOrderValue> {
    PageValue {
        item: page
            .item
            .into_vec()
            .into_iter()
            .map(Into::into)
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        next_cursor: page.next_cursor,
    }
}

fn receipt_page(page: receipt::Page) -> PageValue<ReceiptValue> {
    PageValue {
        item: page
            .item
            .into_vec()
            .into_iter()
            .map(Into::into)
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        next_cursor: page.next_cursor,
    }
}

#[derive(Debug, Serialize)]
struct RecordReceiptResultValue {
    receipt_id: Box<str>,
    purchase_order_id: Box<str>,
    purchase_order_status: &'static str,
    row_version: Box<str>,
}

impl From<record_receipt::RecordReceiptResult> for RecordReceiptResultValue {
    fn from(result: record_receipt::RecordReceiptResult) -> Self {
        Self {
            receipt_id: result.receipt_id,
            purchase_order_id: result.purchase_order_id,
            purchase_order_status: result.purchase_order_status.as_str(),
            row_version: result.row_version.to_string().into_boxed_str(),
        }
    }
}

fn parse_item<T: DeserializeOwned>(item: &EnvelopeItem) -> Result<T, OperationError> {
    serde_json::from_value(item.body.clone()).map_err(|_| invalid_input("input"))
}

fn invalid_input(field: &'static str) -> OperationError {
    OperationError {
        code: "invalid_input",
        detail: ErrorDetail::InvalidInput(InvalidInputDetail {
            field,
            minimum: None,
            maximum: None,
            observed: None,
        }),
    }
}

fn access_error(
    error: &AccessError,
    operation: &'static str,
    id: Option<&str>,
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
            ErrorDetail::InvalidInput(InvalidInputDetail {
                field,
                minimum: error.minimum(),
                maximum: error.maximum(),
                observed: error.observed(),
            })
        }
        AccessErrorKind::NotFound => {
            let Some(id) = id else {
                return internal();
            };
            ErrorDetail::FieldId(FieldIdDetail {
                field: "id",
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
        AccessErrorKind::UniqueViolation
        | AccessErrorKind::ForeignKeyViolation
        | AccessErrorKind::CheckViolation => {
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

fn record_receipt_error(error: &RecordReceiptError) -> OperationError {
    let empty = || ErrorDetail::Empty(EmptyDetail {});
    let internal = || OperationError {
        code: "internal_error",
        detail: empty(),
    };
    let detail = match error.kind() {
        RecordReceiptErrorKind::InvalidInput => {
            let Some(field) = error.field() else {
                return internal();
            };
            ErrorDetail::InvalidInput(InvalidInputDetail {
                field,
                minimum: error.minimum().and_then(|value| i64::try_from(value).ok()),
                maximum: error.maximum().and_then(|value| i64::try_from(value).ok()),
                observed: error.observed().and_then(|value| i64::try_from(value).ok()),
            })
        }
        RecordReceiptErrorKind::PurchaseOrderNotFound
        | RecordReceiptErrorKind::PurchaseOrderNotOpen
        | RecordReceiptErrorKind::IdempotencyConflict => {
            let Some(field) = error.field() else {
                return internal();
            };
            ErrorDetail::Field(FieldDetail { field })
        }
        RecordReceiptErrorKind::PurchaseOrderLineNotFound
        | RecordReceiptErrorKind::PurchaseOrderLineMismatch
        | RecordReceiptErrorKind::LocationNotFound
        | RecordReceiptErrorKind::QuantityExceedsRemaining => {
            let (Some(field), Some(id)) = (error.field(), error.id()) else {
                return internal();
            };
            ErrorDetail::FieldId(FieldIdDetail {
                field,
                id: id.into(),
            })
        }
        RecordReceiptErrorKind::ReceiptReferenceConflict => {
            let Some(constraint) = error.constraint() else {
                return internal();
            };
            ErrorDetail::Constraint(ConstraintDetail {
                constraint: constraint.into(),
            })
        }
        RecordReceiptErrorKind::PermissionDenied => ErrorDetail::Permission(PermissionDetail {
            operation: "receiving.record_receipt",
        }),
        RecordReceiptErrorKind::Retry
        | RecordReceiptErrorKind::Timeout
        | RecordReceiptErrorKind::InternalError => empty(),
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

/// Execute only `purchase_order.get`.
pub async fn purchase_order_get(input: &str) -> Result<String, InvocationError> {
    let items = prepare_envelope(input)?;
    let mut connection = WamnConnection::new();
    let mut output: Vec<ItemResult<PurchaseOrderValue>> = Vec::with_capacity(items.len());
    for item in items.into_vec() {
        let parsed = match parse_item::<GetInput>(&item) {
            Ok(parsed) => parsed,
            Err(error) => {
                output.push(refused(item.request_id, error));
                continue;
            }
        };
        let result = purchase_order::get(&mut connection, &parsed.id).await;
        output.push(match result {
            Ok(value) => ItemResult::Succeeded {
                request_id: item.request_id,
                value: value.into(),
            },
            Err(error) => refused(
                item.request_id,
                access_error(&error, "purchase_order.get", Some(&parsed.id), None),
            ),
        });
    }
    Ok(serialized(&output))
}

/// Execute only `purchase_order.query`.
pub async fn purchase_order_query(input: &str) -> Result<String, InvocationError> {
    let items = prepare_envelope(input)?;
    let mut connection = WamnConnection::new();
    let mut output: Vec<ItemResult<PageValue<PurchaseOrderValue>>> =
        Vec::with_capacity(items.len());
    for item in items.into_vec() {
        let parsed = match parse_item::<PurchaseOrderQueryInput>(&item) {
            Ok(parsed) => purchase_order::QueryInput::from(parsed),
            Err(error) => {
                output.push(refused(item.request_id, error));
                continue;
            }
        };
        let result = purchase_order::query(&mut connection, &parsed).await;
        output.push(match result {
            Ok(value) => ItemResult::Succeeded {
                request_id: item.request_id,
                value: purchase_order_page(value),
            },
            Err(error) => refused(
                item.request_id,
                access_error(&error, "purchase_order.query", None, None),
            ),
        });
    }
    Ok(serialized(&output))
}

/// Execute only `purchase_order.update`.
pub async fn purchase_order_update(input: &str) -> Result<String, InvocationError> {
    let items = prepare_envelope(input)?;
    let mut connection = WamnConnection::new();
    let mut output: Vec<ItemResult<PurchaseOrderValue>> = Vec::with_capacity(items.len());
    for item in items.into_vec() {
        let parsed = match parse_item::<PurchaseOrderUpdateInput>(&item) {
            Ok(parsed) => parsed,
            Err(error) => {
                output.push(refused(item.request_id, error));
                continue;
            }
        };
        let Ok(expected_row_version) = parse_int64(&parsed.expected_row_version) else {
            output.push(refused(
                item.request_id,
                invalid_input("expected_row_version"),
            ));
            continue;
        };
        let result = purchase_order::update(
            &mut connection,
            &parsed.id,
            expected_row_version,
            parsed.change.supplier_id.into(),
        )
        .await;
        output.push(match result {
            Ok(value) => ItemResult::Succeeded {
                request_id: item.request_id,
                value: value.into(),
            },
            Err(error) => refused(
                item.request_id,
                access_error(
                    &error,
                    "purchase_order.update",
                    Some(&parsed.id),
                    Some(expected_row_version),
                ),
            ),
        });
    }
    Ok(serialized(&output))
}

/// Execute only `receipt.get`.
pub async fn receipt_get(input: &str) -> Result<String, InvocationError> {
    let items = prepare_envelope(input)?;
    let mut connection = WamnConnection::new();
    let mut output: Vec<ItemResult<ReceiptValue>> = Vec::with_capacity(items.len());
    for item in items.into_vec() {
        let parsed = match parse_item::<GetInput>(&item) {
            Ok(parsed) => parsed,
            Err(error) => {
                output.push(refused(item.request_id, error));
                continue;
            }
        };
        let result = receipt::get(&mut connection, &parsed.id).await;
        output.push(match result {
            Ok(value) => ItemResult::Succeeded {
                request_id: item.request_id,
                value: value.into(),
            },
            Err(error) => refused(
                item.request_id,
                access_error(&error, "receipt.get", Some(&parsed.id), None),
            ),
        });
    }
    Ok(serialized(&output))
}

/// Execute only `receipt.query`.
pub async fn receipt_query(input: &str) -> Result<String, InvocationError> {
    let items = prepare_envelope(input)?;
    let mut connection = WamnConnection::new();
    let mut output: Vec<ItemResult<PageValue<ReceiptValue>>> = Vec::with_capacity(items.len());
    for item in items.into_vec() {
        let parsed = match parse_item::<ReceiptQueryInput>(&item) {
            Ok(parsed) => receipt::QueryInput::from(parsed),
            Err(error) => {
                output.push(refused(item.request_id, error));
                continue;
            }
        };
        let result = receipt::query(&mut connection, &parsed).await;
        output.push(match result {
            Ok(value) => ItemResult::Succeeded {
                request_id: item.request_id,
                value: receipt_page(value),
            },
            Err(error) => refused(
                item.request_id,
                access_error(&error, "receipt.query", None, None),
            ),
        });
    }
    Ok(serialized(&output))
}

/// Execute only `receiving.record_receipt`.
pub async fn receiving_record_receipt(input: &str) -> Result<String, InvocationError> {
    let items = prepare_envelope(input)?;
    let mut connection = WamnConnection::new();
    let mut output: Vec<ItemResult<RecordReceiptResultValue>> = Vec::with_capacity(items.len());
    for item in items.into_vec() {
        let parsed = match parse_item::<RecordReceiptInput>(&item) {
            Ok(parsed) => parsed,
            Err(error) => {
                output.push(refused(item.request_id, error));
                continue;
            }
        };
        let command = [record_receipt_input(item.request_id.clone(), parsed)];
        let result = record_receipt::record_receipt(&mut connection, &command).await;
        let result = match result {
            Ok(outcome) => outcome
                .into_vec()
                .pop()
                .expect("one command yields one correlated result"),
            Err(error) => {
                output.push(refused(item.request_id, record_receipt_error(&error)));
                continue;
            }
        };
        output.push(match result {
            record_receipt::RecordReceiptItemOutcome::Succeeded { request_id, value } => {
                ItemResult::Succeeded {
                    request_id,
                    value: value.into(),
                }
            }
            record_receipt::RecordReceiptItemOutcome::Refused { request_id, error } => {
                refused(request_id, record_receipt_error(&error))
            }
        });
    }
    Ok(serialized(&output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_postgres_sqlx::{TimestampTz, Uuid};

    #[test]
    fn envelope_requires_all_request_ids_before_item_processing() {
        let input = r#"[
            {"request_id":"first","id":"00000000-0000-0000-0000-000000000001"},
            {"id":"00000000-0000-0000-0000-000000000002"}
        ]"#;

        let error = prepare_envelope(input).expect_err("a missing request_id must refuse");
        assert_eq!(error.code(), "invalid_input");
        assert_eq!(
            error.context(),
            "every operation item must carry a nonempty string request_id"
        );
    }

    #[test]
    fn envelope_preserves_order_and_removes_only_correlation_identity() {
        let input = r#"[
            {"request_id":"second","id":"00000000-0000-0000-0000-000000000002"},
            {"request_id":"first","id":"00000000-0000-0000-0000-000000000001"}
        ]"#;

        let items = prepare_envelope(input).expect("the envelope is valid");
        assert_eq!(items[0].request_id.as_ref(), "second");
        assert_eq!(items[1].request_id.as_ref(), "first");
        assert!(
            items
                .iter()
                .all(|item| item.body.get("request_id").is_none())
        );
    }

    #[test]
    fn dto_unknown_fields_and_noncanonical_int64_refuse_in_memory() {
        assert!(
            serde_json::from_value::<GetInput>(serde_json::json!({
                "id": "00000000-0000-0000-0000-000000000001",
                "future": true,
            }))
            .is_err()
        );
        assert_eq!(parse_int64("0"), Ok(0));
        assert_eq!(parse_int64("-42"), Ok(-42));
        for value in ["", "01", "-0", "+1", "1.0"] {
            assert_eq!(parse_int64(value), Err(()));
        }
    }

    #[test]
    fn update_preserves_omitted_null_and_value_states() {
        let omitted: PurchaseOrderUpdateInput = serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "expected_row_version": "1",
            "change": {}
        }))
        .unwrap();
        assert!(matches!(omitted.change.supplier_id, Nullable::Omitted));

        let explicit_null: PurchaseOrderUpdateInput = serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "expected_row_version": "1",
            "change": {"supplier_id": null}
        }))
        .unwrap();
        assert!(matches!(explicit_null.change.supplier_id, Nullable::Null));
    }

    #[test]
    fn persisted_int64_and_structured_errors_have_closed_wire_shapes() {
        let value = PurchaseOrderValue::from(purchase_order::PurchaseOrderRow {
            created_at: TimestampTz("2026-08-31T12:00:00.000000Z".to_owned()),
            id: Uuid("00000000-0000-0000-0000-000000000001".to_owned()),
            purchase_order_number: "PO-1".to_owned(),
            row_version: 42,
            status: "open".to_owned(),
            supplier_id: Uuid("00000000-0000-0000-0000-000000000002".to_owned()),
            updated_at: TimestampTz("2026-08-31T12:01:00.000000Z".to_owned()),
        });
        assert_eq!(
            serde_json::to_value(value).expect("the result serializes")["row_version"],
            "42"
        );

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
                "detail": {
                    "expected_row_version": "4",
                    "observed_row_version": "5"
                }
            })
        );
    }
}
