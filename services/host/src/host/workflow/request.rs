//! Deliver the exact native Request path on the application workload.

use std::{sync::LazyLock, time::Duration};

use http_body_util::{BodyExt as _, LengthLimitError, Limited};
use hyper::{Method, Request, Response, StatusCode, header};
use serde_json::json;
use tokio::time::Instant;
use wamn_application::ingress::{Header, RoutePolicy};
use wamn_engine::ingress::IngressRouting;
use wamn_package::canonical_name::CanonicalName;
use wasmtime_wasi_http::WasiBody;

use super::{Refusal, RequestCall, Workflow};

const DEADLINE: Duration = Duration::from_secs(30);
const MAX_BODY_BYTES: usize = 1024 * 1024;
static ATTACHMENT: LazyLock<CanonicalName> = LazyLock::new(|| {
    "wamn-workflow-request-http"
        .parse()
        .expect("fixed attachment id")
});
static POLICY: LazyLock<serde_json::Value> = LazyLock::new(|| json!({"modes": ["pat", "session"]}));

impl Workflow {
    pub(in crate::host) async fn http(
        &self,
        routing: &IngressRouting,
        workload: &str,
        prefix: &str,
        request: Request<WasiBody>,
    ) -> Response<WasiBody> {
        let deadline = Instant::now() + DEADLINE;
        tokio::time::timeout_at(deadline, async {
            if request.method() != Method::POST {
                return error(StatusCode::METHOD_NOT_ALLOWED, "method-not-allowed");
            }
            let Ok(state) = self.workload(workload) else {
                return error(StatusCode::SERVICE_UNAVAILABLE, "workflow-unavailable");
            };
            let (parts, body) = request.into_parts();
            let Some(path) = parts
                .uri
                .path()
                .strip_prefix(prefix)
                .filter(|path| path.starts_with('/'))
            else {
                return error(StatusCode::NOT_FOUND, "workflow-not-found");
            };
            let headers = parts
                .headers
                .get_all(header::AUTHORIZATION)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .map(|value| Header {
                    name: header::AUTHORIZATION.to_string(),
                    value: value.to_owned(),
                })
                .collect::<Vec<_>>();
            let caller = match routing
                .authenticate_policy(
                    &state.project_key,
                    &ATTACHMENT,
                    RoutePolicy { policy: &POLICY },
                    &headers,
                    &[],
                )
                .await
            {
                Ok(Some(caller)) => caller,
                Ok(None) => return error(StatusCode::UNAUTHORIZED, "unauthorized"),
                Err(rejection) => {
                    return error(
                        StatusCode::from_u16(rejection.status)
                            .expect("authentication refusal status"),
                        &rejection.code,
                    );
                }
            };
            let bytes = match Limited::new(body, MAX_BODY_BYTES).collect().await {
                Ok(body) => body.to_bytes(),
                Err(failure) if failure.is::<LengthLimitError>() => {
                    return error(StatusCode::PAYLOAD_TOO_LARGE, "body-too-large");
                }
                Err(_) => return error(StatusCode::BAD_REQUEST, "invalid-body"),
            };
            let Ok(input) = serde_json::from_slice(&bytes) else {
                return error(StatusCode::BAD_REQUEST, "malformed-json");
            };
            match self
                .call(RequestCall {
                    workload,
                    path,
                    caller: &caller,
                    input,
                    deadline,
                })
                .await
            {
                Ok(Some(response))
                    if (200..=599).contains(&response.status)
                        && serde_json::from_str::<serde::de::IgnoredAny>(&response.body_json)
                            .is_ok() =>
                {
                    let status =
                        StatusCode::from_u16(response.status).expect("valid response status");
                    answer(status, response.body_json)
                }
                Ok(None) => error(StatusCode::NOT_FOUND, "workflow-not-found"),
                Err(Refusal::ActiveRoleRequired) => {
                    error(StatusCode::FORBIDDEN, "active_role_required")
                }
                Err(Refusal::PermissionDenied) => error(StatusCode::FORBIDDEN, "permission-denied"),
                Err(Refusal::DeadlineElapsed) => {
                    error(StatusCode::GATEWAY_TIMEOUT, "deadline-elapsed")
                }
                Ok(Some(_)) | Err(_) => {
                    error(StatusCode::SERVICE_UNAVAILABLE, "workflow-unavailable")
                }
            }
        })
        .await
        .unwrap_or_else(|_| error(StatusCode::GATEWAY_TIMEOUT, "deadline-elapsed"))
    }
}

fn error(status: StatusCode, code: &str) -> Response<WasiBody> {
    answer(status, json!({"error": {"code": code}}).to_string())
}

fn answer(status: StatusCode, json: String) -> Response<WasiBody> {
    let mut response = Response::new(
        http_body_util::Full::new(bytes::Bytes::from(json))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}
