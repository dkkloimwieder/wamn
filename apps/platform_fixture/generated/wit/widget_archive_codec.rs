// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::ArchiveItem;
const MINIMUM: usize = 1;
const MAXIMUM: usize = 100;
const COUNT_ERROR: &str = "operation input item count must be 1..=100";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    expected_edit_version: JsonInt64,
    id: String,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::ArchiveItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::ArchiveRequest {
                    expected_edit_version: request.expected_edit_version.0,
                    id: request.id,
                })
                .map_err(|_| invalid("input"));
            Ok(contract::ArchiveItem { request_id, input })
        })
        .collect()
}

fn invalid(field: &str) -> contract::InvalidInputDetail {
    contract::InvalidInputDetail {
        field: field.to_owned(),
    }
}

pub(crate) fn encode(output: &[contract::ArchiveOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({
                "request_id": item.request_id,
                "value": {
                    "id": value.id,
                    "edit_version": value.edit_version.to_string(),
                    "note": value.note,
                }
            }),
            Err(error) => json!({
                "request_id": item.request_id,
                "error": error_value(error),
            }),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed outcomes always serialize")
}

fn error_value(error: &contract::ArchiveError) -> Value {
    let (code, detail) = match error {
        contract::ArchiveError::InvalidInput(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("invalid_input", detail)
        }
        contract::ArchiveError::AlreadyArchived(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("already_archived", detail)
        }
        contract::ArchiveError::ConcurrencyConflict(value) => {
            let mut detail = Map::new();
            detail.insert(
                "expected_row_version".to_owned(),
                json!(JsonInt64(value.expected_row_version)),
            );
            detail.insert(
                "observed_row_version".to_owned(),
                json!(JsonInt64(value.observed_row_version)),
            );
            ("concurrency_conflict", detail)
        }
        contract::ArchiveError::Retry => ("retry", Map::new()),
        contract::ArchiveError::Timeout => ("timeout", Map::new()),
        contract::ArchiveError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::ArchiveError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(request: &mut contract::ArchiveRequest) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    {
        let value = &mut request.id;
        if !canonical_uuid(value) {
            return Err(invalid("id"));
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    input: Vec<contract::ArchiveItem>,
    state: &mut S,
    mut handler: F,
) -> Vec<contract::ArchiveOutcome>
where
    F: AsyncFnMut(
        &mut S,
        contract::ArchiveRequest,
    ) -> Result<contract::ArchiveResult, contract::ArchiveError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => handler(state, request).await,
                Err(error) => Err(contract::ArchiveError::InvalidInput(error)),
            },
            Err(error) => Err(contract::ArchiveError::InvalidInput(error)),
        };
        output.push(contract::ArchiveOutcome {
            request_id: item.request_id,
            outcome,
        });
    }
    output
}

#[allow(unused_macros)]
macro_rules! row {
    ($row:expr, $target:path) => {{
        let row = $row;
        $target {
            id: row.id.0,
            edit_version: row.edit_version,
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
) -> contract::ArchiveError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::ArchiveError::InternalError;
            };
            contract::ArchiveError::InvalidInput(contract::InvalidInputDetail { field })
        }
        "already_archived" => {
            let Some(field) = detail("field") else {
                return contract::ArchiveError::InternalError;
            };
            contract::ArchiveError::AlreadyArchived(contract::AlreadyArchivedDetail { field })
        }
        "concurrency_conflict" => {
            let Some(expected_row_version) =
                detail("expected_row_version").and_then(|value| value.parse::<i64>().ok())
            else {
                return contract::ArchiveError::InternalError;
            };
            let Some(observed_row_version) =
                detail("observed_row_version").and_then(|value| value.parse::<i64>().ok())
            else {
                return contract::ArchiveError::InternalError;
            };
            contract::ArchiveError::ConcurrencyConflict(contract::ConcurrencyConflictDetail {
                expected_row_version,
                observed_row_version,
            })
        }
        "retry" => contract::ArchiveError::Retry,
        "timeout" => contract::ArchiveError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::ArchiveError::InternalError;
            };
            contract::ArchiveError::PermissionDenied(contract::PermissionDeniedDetail { operation })
        }
        _ => contract::ArchiveError::InternalError,
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
                    input: Vec<__contract::ArchiveItem>,
                ) -> Result<Vec<__contract::ArchiveOutcome>, __node::NodeError> {
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
