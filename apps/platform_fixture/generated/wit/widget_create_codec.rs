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
    code: JsonChange<String>,
    #[serde(default)]
    maker_id: JsonChange<String>,
    #[serde(default)]
    note: JsonChange<String>,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::CreateItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::CreateRequest {
                    idempotency_key: request.idempotency_key,
                    code: change(request.code),
                    maker_id: change(request.maker_id),
                    note: change(request.note),
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
                                "code": value.value.code,
                                "created_at": value.value.created_at,
                                "edit_version": value.value.edit_version.to_string(),
                                "id": value.value.id,
                                "maker_id": value.value.maker_id,
                                "note": value.value.note,
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
                "widget_code_key" => Some("code"),
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
                "widget_maker_id_fkey" => Some("maker_id"),
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
    if request.code.is_none() {
        return Err(invalid("code"));
    }
    if matches!(request.code, Some(None)) {
        return Err(invalid("code"));
    }
    if let Some(Some(value)) = &mut request.code
        && (!["priority", "standard"].contains(&value.as_str()))
    {
        return Err(invalid("code"));
    }
    if let Some(Some(value)) = &mut request.maker_id
        && (!canonical_uuid(value))
    {
        return Err(invalid("maker_id"));
    }
    Ok(())
}

include!("write_log_codec.rs");

/// The write log operation of this contract: its operation without the version.
const OPERATION: &str = "platform-fixture:widget/create";
/// The input field that carries the key the write log claims.
const KEY_FIELD: &str = "idempotency_key";

/// The bytes the write log keeps for one validated request: its canonical JSON
/// without the key and the request id.
fn request_bytes(request: &contract::CreateRequest) -> Vec<u8> {
    wamn_execution_contract::canonical_json_bytes(&{
        let mut value = Map::new();
        if let Some(field) = &request.code {
            value.insert("code".to_owned(), json!(field));
        }
        if let Some(field) = &request.maker_id {
            value.insert("maker_id".to_owned(), json!(field));
        }
        if let Some(field) = &request.note {
            value.insert("note".to_owned(), json!(field));
        }
        Value::Object(value)
    })
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
    code: String,
    created_at: String,
    edit_version: JsonInt64,
    id: String,
    maker_id: Option<String>,
    note: Option<String>,
}

/// The success that one stored result answers.
fn stored_success(result: &str) -> Option<contract::CreateResult> {
    let value: JsonResult = serde_json::from_str(result).ok()?;
    Some(contract::CreateResult {
        value: contract::CreateRow {
            code: value.code,
            created_at: value.created_at,
            edit_version: value.edit_version.0,
            id: value.id,
            maker_id: value.maker_id,
            note: value.note,
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
    let bytes = request_bytes(&request);
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
            code: row.code,
            created_at: row.created_at.0,
            edit_version: row.edit_version,
            id: row.id.0,
            maker_id: row.maker_id.map(|value| value.0),
            note: row.note,
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
