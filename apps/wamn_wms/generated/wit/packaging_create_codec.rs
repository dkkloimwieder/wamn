// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::CreateItem;
const MINIMUM: usize = 1;
const MAXIMUM: usize = 100;
const COUNT_ERROR: &str = "operation input item count must be 1..=100";

#[allow(dead_code)]
pub(crate) fn validate(input: &[Item]) -> Result<(), CodecError> {
    validate_count(input.len())?;
    if input.iter().any(|item| item.request_id.is_empty()) {
        return Err(CodecError(
            "every operation item must carry a nonempty string request_id",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    idempotency_key: String,
    #[serde(default)]
    packaging_code: JsonChange<String>,
    #[serde(default)]
    r#type: JsonChange<String>,
    #[serde(default)]
    location_id: JsonChange<String>,
    #[serde(default)]
    status: JsonChange<String>,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::CreateItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::CreateRequest {
                    idempotency_key: request.idempotency_key,
                    packaging_code: change(request.packaging_code),
                    type_: change(request.r#type),
                    location_id: change(request.location_id),
                    status: change(request.status),
                })
                .map_err(|_| invalid("input"));
            Ok(contract::CreateItem { request_id, input })
        })
        .collect()
}

fn invalid(field: &str) -> contract::InvalidInputDetail {
    contract::InvalidInputDetail {
        field: field.to_owned(),
    }
}

pub(crate) fn encode(output: &[contract::CreateOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({ "request_id": item.request_id, "value":
            json!({
                                "created_at": value.value.created_at,
                                "created_by": value.value.created_by,
                                "id": value.value.id,
                                "location_id": value.value.location_id,
                                "packaging_code": value.value.packaging_code,
                                "row_version": value.value.row_version,
                                "status": value.value.status,
                                "type": value.value.type_,
                                "updated_at": value.value.updated_at,
                                "updated_by": value.value.updated_by,
                        })
                    }),
            Err(error) => json!({ "request_id": item.request_id, "error": error_value(error) }),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed operation outcomes always serialize")
}

fn error_value(error: &contract::CreateError) -> Value {
    let (code, detail) = match error {
        contract::CreateError::InvalidInput(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("invalid_input", detail)
        }
        contract::CreateError::IdempotencyConflict(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("idempotency_conflict", detail)
        }
        contract::CreateError::UniqueViolation(value) => {
            let mut detail = Map::new();
            detail.insert("constraint".to_owned(), json!(value.constraint));
            if let Some(field) = match value.constraint.as_str() {
                "packaging_packaging_code_key" => Some("packaging_code"),
                _ => None,
            } {
                detail.insert("field".to_owned(), json!(field));
            }
            ("unique_violation", detail)
        }
        contract::CreateError::ForeignKeyViolation(value) => {
            let mut detail = Map::new();
            detail.insert("constraint".to_owned(), json!(value.constraint));
            if let Some(field) = match value.constraint.as_str() {
                "packaging_location_id_fkey" => Some("location_id"),
                _ => None,
            } {
                detail.insert("field".to_owned(), json!(field));
            }
            ("foreign_key_violation", detail)
        }
        contract::CreateError::CheckViolation(value) => {
            let mut detail = Map::new();
            detail.insert("constraint".to_owned(), json!(value.constraint));
            ("check_violation", detail)
        }
        contract::CreateError::Retry => ("retry", Map::new()),
        contract::CreateError::Timeout => ("timeout", Map::new()),
        contract::CreateError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::CreateError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(request: &mut contract::CreateRequest) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    if request.packaging_code.is_none() {
        return Err(invalid("packaging_code"));
    }
    if matches!(request.packaging_code, Some(None)) {
        return Err(invalid("packaging_code"));
    }
    if request.type_.is_none() {
        return Err(invalid("type"));
    }
    if matches!(request.type_, Some(None)) {
        return Err(invalid("type"));
    }
    if let Some(Some(value)) = &mut request.type_
        && (!["pallet", "tote", "bin", "case", "loose"].contains(&value.as_str()))
    {
        return Err(invalid("type"));
    }
    if request.location_id.is_none() {
        return Err(invalid("location_id"));
    }
    if matches!(request.location_id, Some(None)) {
        return Err(invalid("location_id"));
    }
    if let Some(Some(value)) = &mut request.location_id
        && (!canonical_uuid(value))
    {
        return Err(invalid("location_id"));
    }
    if request.status.is_none() {
        return Err(invalid("status"));
    }
    if matches!(request.status, Some(None)) {
        return Err(invalid("status"));
    }
    if let Some(Some(value)) = &mut request.status
        && (!["available", "held", "consumed"].contains(&value.as_str()))
    {
        return Err(invalid("status"));
    }
    Ok(())
}

include!("write_log_codec.rs");

/// The write log operation of this contract: its operation without the version.
const OPERATION: &str = "wamn-wms:packaging/create";
/// The input field that carries the key the write log claims.
const KEY_FIELD: &str = "idempotency_key";

/// The bytes the write log keeps for one validated request: its canonical JSON
/// without the key and the request id, and the participation intent when one
/// is selected.
fn request_bytes(request: &contract::CreateRequest, intent: Option<&str>) -> Vec<u8> {
    let mut value = {
        let mut value = Map::new();
        if let Some(field) = &request.packaging_code {
            value.insert("packaging_code".to_owned(), json!(&field));
        }
        if let Some(field) = &request.type_ {
            value.insert("type".to_owned(), json!(&field));
        }
        if let Some(field) = &request.location_id {
            value.insert("location_id".to_owned(), json!(&field));
        }
        if let Some(field) = &request.status {
            value.insert("status".to_owned(), json!(&field));
        }
        Value::Object(value)
    };
    if let (Some(intent), Value::Object(object)) = (intent, &mut value) {
        object.insert("participation_intent".to_owned(), json!(intent));
    }
    wamn_execution_contract::canonical_json_bytes(&value)
}

/// The result the write log stores for one success: its encoded outcome
/// without the request id.
fn stored_result(result: &contract::CreateResult) -> String {
    let encoded = encode(&[contract::CreateOutcome {
        request_id: String::new(),
        outcome: Ok(result.clone()),
    }]);
    let mut outcomes: Vec<Value> =
        serde_json::from_str(&encoded).expect("the encoder writes a JSON list");
    outcomes
        .pop()
        .and_then(|mut outcome| outcome.get_mut("value").map(Value::take))
        .expect("an encoded success carries its value")
        .to_string()
}

#[derive(Deserialize)]
struct JsonResult {
    created_at: String,
    created_by: String,
    id: String,
    location_id: String,
    packaging_code: String,
    row_version: i32,
    status: String,
    r#type: String,
    updated_at: String,
    updated_by: String,
}

/// The success that one stored result answers.
fn stored_success(result: &str) -> Option<contract::CreateResult> {
    let value: JsonResult = serde_json::from_str(result).ok()?;
    Some(contract::CreateResult {
        value: contract::CreateRow {
            created_at: value.created_at,
            created_by: value.created_by,
            id: value.id,
            location_id: value.location_id,
            packaging_code: value.packaging_code,
            row_version: value.row_version,
            status: value.status,
            type_: value.r#type,
            updated_at: value.updated_at,
            updated_by: value.updated_by,
        },
    })
}

#[allow(dead_code)]
pub(crate) async fn run<F>(
    input: Vec<contract::CreateItem>,
    connection: &mut wamn_postgres_statements::Connection,
    mut handler: F,
) -> Vec<contract::CreateOutcome>
where
    F: AsyncFnMut(
        &mut wamn_postgres_statements::Transaction,
        contract::CreateRequest,
    ) -> Result<contract::CreateResult, contract::CreateError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => claimed(connection, request, &mut handler).await,
                Err(error) => Err(contract::CreateError::InvalidInput(error)),
            },
            Err(error) => Err(contract::CreateError::InvalidInput(error)),
        };
        output.push(contract::CreateOutcome {
            request_id: item.request_id,
            outcome,
        });
    }
    output
}

/// Claim the key of one request, do its work and store its result in one
/// transaction, or answer what a committed claim of the key holds.
async fn claimed<F>(
    connection: &mut wamn_postgres_statements::Connection,
    request: contract::CreateRequest,
    handler: &mut F,
) -> Result<contract::CreateResult, contract::CreateError>
where
    F: AsyncFnMut(
        &mut wamn_postgres_statements::Transaction,
        contract::CreateRequest,
    ) -> Result<contract::CreateResult, contract::CreateError>,
{
    let refuse =
        |error: &wamn_postgres_statements::StatementError| map_error(log_error(error), |_| None);
    let key = request.idempotency_key.clone();
    let intent: Option<String> = None;
    let bytes = request_bytes(&request, intent.as_deref());
    let mut transaction = connection.begin().await.map_err(|error| refuse(&error))?;
    match log_claim(&mut transaction, OPERATION, &key, &bytes).await {
        Ok(Logged::Claimed) => {}
        Ok(Logged::Stored(stored, result)) => {
            let _ = transaction.rollback().await;
            if stored != bytes {
                return Err(map_error("idempotency_conflict", |name| {
                    (name == "field").then(|| KEY_FIELD.to_owned())
                }));
            }
            return result
                .as_deref()
                .and_then(stored_success)
                .ok_or_else(|| map_error("internal_error", |_| None));
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            return Err(refuse(&error));
        }
    }
    let result = match handler(&mut transaction, request).await {
        Ok(result) => result,
        Err(error) => {
            let _ = transaction.rollback().await;
            return Err(error);
        }
    };
    if let Err(error) = log_finish(&mut transaction, OPERATION, &key, stored_result(&result)).await
    {
        let _ = transaction.rollback().await;
        return Err(refuse(&error));
    }
    transaction.commit().await.map_err(|error| refuse(&error))?;
    Ok(result)
}

#[allow(unused_macros)]
macro_rules! row {
    ($row:expr, $target:path) => {{
        let row = $row;
        $target {
            created_at: row.created_at.0,
            created_by: row.created_by.0,
            id: row.id.0,
            location_id: row.location_id.0,
            packaging_code: row.packaging_code,
            row_version: row.row_version,
            status: row.status,
            type_: row.r#type,
            updated_at: row.updated_at.0,
            updated_by: row.updated_by.0,
        }
    }};
}
#[allow(unused_imports)]
pub(crate) use row;

#[allow(dead_code)]
pub(crate) fn map_error(
    code: &str,
    mut detail: impl FnMut(&str) -> Option<String>,
) -> contract::CreateError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::CreateError::InternalError;
            };
            contract::CreateError::InvalidInput(contract::InvalidInputDetail { field })
        }
        "idempotency_conflict" => {
            let Some(field) = detail("field") else {
                return contract::CreateError::InternalError;
            };
            contract::CreateError::IdempotencyConflict(contract::IdempotencyConflictDetail {
                field,
            })
        }
        "unique_violation" => {
            let Some(constraint) = detail("constraint") else {
                return contract::CreateError::InternalError;
            };
            contract::CreateError::UniqueViolation(contract::UniqueViolationDetail { constraint })
        }
        "foreign_key_violation" => {
            let Some(constraint) = detail("constraint") else {
                return contract::CreateError::InternalError;
            };
            contract::CreateError::ForeignKeyViolation(contract::ForeignKeyViolationDetail {
                constraint,
            })
        }
        "check_violation" => {
            let Some(constraint) = detail("constraint") else {
                return contract::CreateError::InternalError;
            };
            contract::CreateError::CheckViolation(contract::CheckViolationDetail { constraint })
        }
        "retry" => contract::CreateError::Retry,
        "timeout" => contract::CreateError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::CreateError::InternalError;
            };
            contract::CreateError::PermissionDenied(contract::PermissionDeniedDetail { operation })
        }
        _ => contract::CreateError::InternalError,
    }
}

#[allow(unused_macros)]
macro_rules! export_operation {
    ($component:ty, $contract:path, $node:path, $state:expr, $handler:path, $codec:ident) => {
        const _: () = {
            use $codec as __codec;
            use $contract as __contract;
            use $node as __node;

            fn invalid(error: __codec::CodecError) -> __node::NodeError {
                __node::NodeError::InvalidInput(__node::ErrorDetail {
                    message: error.context().to_owned(),
                    code: Some("invalid_input".to_owned()),
                })
            }

            impl __contract::Guest for $component {
                async fn run(
                    _context: __node::NodeContext,
                    input: Vec<__contract::CreateItem>,
                ) -> Result<Vec<__contract::CreateOutcome>, __node::NodeError> {
                    let mut state = $state;
                    __codec::validate(&input).map_err(invalid)?;
                    Ok(__codec::run(input, &mut state, $handler).await)
                }

                async fn run_json(
                    context: __node::NodeContext,
                    input: String,
                ) -> Result<__node::Emission, __node::NodeError> {
                    let input = __codec::decode(&input).map_err(invalid)?;
                    let output = <Self as __contract::Guest>::run(context, input).await?;
                    Ok(__node::Emission {
                        payload: __codec::encode(&output),
                        port: None,
                    })
                }
            }
        };
    };
}
#[allow(unused_imports)]
pub(crate) use export_operation;
