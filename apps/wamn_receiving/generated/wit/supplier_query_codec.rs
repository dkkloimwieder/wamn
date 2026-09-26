// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
const MINIMUM: usize = 1;
const MAXIMUM: usize = 1;
const COUNT_ERROR: &str = "a query takes exactly one request";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

pub(crate) fn decode(
    input: &str,
) -> Result<Result<contract::QueryRequest, contract::InvalidInputDetail>, CodecError> {
    let body = decode_read_envelope(input)?
        .into_iter()
        .next()
        .expect("the envelope holds one request");
    Ok(serde_json::from_value::<JsonRequest>(body)
        .map(|request| contract::QueryRequest {
            cursor: request.cursor,
            limit: request.limit,
        })
        .map_err(|_| invalid("input")))
}

fn invalid(field: &str) -> contract::InvalidInputDetail {
    contract::InvalidInputDetail {
        field: field.to_owned(),
        minimum: None,
        maximum: None,
        observed: None,
    }
}

fn row_json(row: &contract::QueryRow) -> String {
    json!({
                    "created_at": row.created_at,
                    "id": row.id,
                    "name": row.name,
    })
    .to_string()
}

/// The outcome after the last row, as the JSON a read reply carries.
pub(crate) fn encode_end(end: &Result<contract::QueryEnd, contract::QueryError>) -> String {
    match end {
        Ok(end) => json!({ "value": { "next_cursor": end.next_cursor } }),
        Err(error) => json!({ "error": error_value(error) }),
    }
    .to_string()
}

fn error_value(error: &contract::QueryError) -> Value {
    let (code, detail) = match error {
        contract::QueryError::InvalidInput(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            if let Some(detail_value) = &value.minimum {
                detail.insert("minimum".to_owned(), json!(detail_value));
            }
            if let Some(detail_value) = &value.maximum {
                detail.insert("maximum".to_owned(), json!(detail_value));
            }
            if let Some(detail_value) = &value.observed {
                detail.insert("observed".to_owned(), json!(detail_value));
            }
            ("invalid_input", detail)
        }
        contract::QueryError::Retry => ("retry", Map::new()),
        contract::QueryError::Timeout => ("timeout", Map::new()),
        contract::QueryError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::QueryError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(request: &mut contract::QueryRequest) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    Ok(())
}

#[allow(clippy::unnecessary_wraps)]
fn limit(request: &mut contract::QueryRequest) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    let limit = *request.limit.get_or_insert(100);
    if limit < 1 {
        #[allow(unused_mut)]
        let mut detail = invalid("limit");
        detail.minimum = Some(1);
        detail.observed = Some(limit);
        return Err(detail);
    }
    Ok(())
}

/// Rows one write hands to the reader.
const BATCH: usize = 500;

enum Writer {
    Typed(wit_bindgen::rt::async_support::StreamWriter<contract::QueryRow>),
    Json(wit_bindgen::rt::async_support::StreamWriter<String>),
}

/// The rows of one read, written to its reader in batches.
pub(crate) struct Rows {
    writer: Writer,
    batch: Vec<contract::QueryRow>,
}

#[allow(dead_code)]
impl Rows {
    pub(crate) fn typed(
        writer: wit_bindgen::rt::async_support::StreamWriter<contract::QueryRow>,
    ) -> Self {
        Self {
            writer: Writer::Typed(writer),
            batch: Vec::with_capacity(BATCH),
        }
    }

    pub(crate) fn json(writer: wit_bindgen::rt::async_support::StreamWriter<String>) -> Self {
        Self {
            writer: Writer::Json(writer),
            batch: Vec::with_capacity(BATCH),
        }
    }

    /// Hand one row to the reader. An error means the reader left, so the
    /// read stops and its outcome reaches no one.
    pub(crate) async fn push(
        &mut self,
        row: contract::QueryRow,
    ) -> Result<(), contract::QueryError> {
        self.batch.push(row);
        if self.batch.len() == BATCH {
            self.flush().await
        } else {
            Ok(())
        }
    }

    async fn flush(&mut self) -> Result<(), contract::QueryError> {
        let batch = std::mem::replace(&mut self.batch, Vec::with_capacity(BATCH));
        if batch.is_empty() {
            return Ok(());
        }
        let unwritten = match &mut self.writer {
            Writer::Typed(writer) => writer.write_all(batch).await.len(),
            Writer::Json(writer) => writer
                .write_all(batch.iter().map(row_json).collect())
                .await
                .len(),
        };
        if unwritten == 0 {
            Ok(())
        } else {
            Err(contract::QueryError::InternalError)
        }
    }
}

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    request: Result<contract::QueryRequest, contract::InvalidInputDetail>,
    state: &mut S,
    rows: &mut Rows,
    mut handler: F,
) -> Result<contract::QueryEnd, contract::QueryError>
where
    F: AsyncFnMut(
        &mut S,
        contract::QueryRequest,
        &mut Rows,
    ) -> Result<contract::QueryEnd, contract::QueryError>,
{
    let mut request = request.map_err(contract::QueryError::InvalidInput)?;
    normalize(&mut request)
        .and_then(|()| limit(&mut request))
        .map_err(contract::QueryError::InvalidInput)?;
    let end = handler(state, request, rows).await?;
    rows.flush().await?;
    Ok(end)
}

#[allow(unused_macros)]
macro_rules! row {
    ($row:expr, $target:path) => {{
        let row = $row;
        $target {
            created_at: row.created_at.0,
            id: row.id.0,
            name: row.name,
        }
    }};
}
#[allow(unused_imports)]
pub(crate) use row;

#[allow(dead_code)]
pub(crate) fn map_error(
    code: &str,
    mut detail: impl FnMut(&str) -> Option<String>,
) -> contract::QueryError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::QueryError::InternalError;
            };
            let minimum = detail("minimum");
            let maximum = detail("maximum");
            let observed = detail("observed");
            contract::QueryError::InvalidInput(contract::InvalidInputDetail {
                field,
                minimum,
                maximum,
                observed,
            })
        }
        "retry" => contract::QueryError::Retry,
        "timeout" => contract::QueryError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::QueryError::InternalError;
            };
            contract::QueryError::PermissionDenied(contract::PermissionDeniedDetail { operation })
        }
        _ => contract::QueryError::InternalError,
    }
}

#[allow(unused_macros)]
macro_rules! export_operation {
    ($component:ty, $contract:path, $node:path, $state:expr, $handler:path, $codec:ident) => {
        const _: () = {
            use wit_bindgen::rt::async_support::{FutureReader, StreamReader, spawn_local};
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
                #[allow(clippy::unused_async_trait_impl)]
                async fn run(
                    _context: __node::NodeContext,
                    input: __contract::QueryRequest,
                ) -> Result<
                    (
                        StreamReader<__contract::QueryRow>,
                        FutureReader<Result<__contract::QueryEnd, __contract::QueryError>>,
                    ),
                    __node::NodeError,
                > {
                    let (writer, rows) = crate::wit_stream::new::<__contract::QueryRow>();
                    let (end, ended) = crate::wit_future::new::<
                        Result<__contract::QueryEnd, __contract::QueryError>,
                    >(|| Err(__contract::QueryError::InternalError));
                    spawn_local(async move {
                        let mut state = $state;
                        let mut sink = __codec::Rows::typed(writer);
                        let outcome =
                            __codec::run(Ok(input), &mut state, &mut sink, $handler).await;
                        drop(sink);
                        let _ = end.write(outcome).await;
                    });
                    Ok((rows, ended))
                }

                #[allow(clippy::unused_async_trait_impl)]
                async fn run_json(
                    _context: __node::NodeContext,
                    input: String,
                ) -> Result<(StreamReader<String>, FutureReader<String>), __node::NodeError>
                {
                    let request = __codec::decode(&input).map_err(invalid)?;
                    let (writer, rows) = crate::wit_stream::new::<String>();
                    let (end, ended) = crate::wit_future::new::<String>(|| {
                        __codec::encode_end(&Err(__contract::QueryError::InternalError))
                    });
                    spawn_local(async move {
                        let mut state = $state;
                        let mut sink = __codec::Rows::json(writer);
                        let outcome = __codec::run(request, &mut state, &mut sink, $handler).await;
                        drop(sink);
                        let _ = end.write(__codec::encode_end(&outcome)).await;
                    });
                    Ok((rows, ended))
                }
            }
        };
    };
}
#[allow(unused_imports)]
pub(crate) use export_operation;
