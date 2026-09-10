//! Only a verified provisioning-operator certificate authorizes PAT issuance.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::{Request, Response, StatusCode, body::Incoming};
use serde::{Deserialize, Serialize};
use wamn_platform_identity::{IdentityErrorKind, IssuedPat, PrincipalId, issue_pat};

use crate::{IO_TIMEOUT, Inner, response, unavailable};

// One principal ID, a library-bounded label, and an integer lifetime fit here.
const MAX_REQUEST_BYTES: usize = 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IssueRequest {
    principal_id: String,
    label: String,
    lifetime_seconds: u64,
}

#[derive(Serialize)]
struct IssueResponse<'a> {
    token: &'a str,
    token_prefix: &'a str,
    principal_id: &'a str,
    created_at: &'a str,
    expires_at: &'a str,
}

pub(super) async fn respond(
    inner: &Inner,
    request: Request<Incoming>,
    operator: bool,
) -> Response<Full<Bytes>> {
    if !operator {
        return response(
            StatusCode::FORBIDDEN,
            "application/json",
            b"{\"error\":\"operator certificate required\"}".to_vec(),
        );
    }
    match tokio::time::timeout(IO_TIMEOUT, issue(inner, request)).await {
        Ok(response) => response,
        Err(_) => unavailable(),
    }
}

async fn issue(inner: &Inner, request: Request<Incoming>) -> Response<Full<Bytes>> {
    let (parts, body) = request.into_parts();
    let mut content_types = parts.headers.get_all(hyper::header::CONTENT_TYPE).iter();
    let content_type = content_types.next().and_then(|value| value.to_str().ok());
    if content_types.next().is_some()
        || !content_type.is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        return invalid_request();
    }
    let Ok(body) = Limited::new(body, MAX_REQUEST_BYTES).collect().await else {
        return invalid_request();
    };
    let Ok(request) = serde_json::from_slice::<IssueRequest>(&body.to_bytes()) else {
        return invalid_request();
    };
    let Ok(principal) = request.principal_id.parse::<PrincipalId>() else {
        return invalid_request();
    };
    match issue_pat(
        &inner.database.client,
        &principal,
        &request.label,
        Duration::from_secs(request.lifetime_seconds),
    )
    .await
    {
        Ok(token) => token_response(&token, &principal),
        Err(error) => match error.kind() {
            IdentityErrorKind::InvalidInput | IdentityErrorKind::NotFound => invalid_request(),
            _ => unavailable(),
        },
    }
}

fn token_response(token: &IssuedPat, principal: &PrincipalId) -> Response<Full<Bytes>> {
    match serde_json::to_vec(&IssueResponse {
        token: token.token(),
        token_prefix: token.record().prefix(),
        principal_id: principal.as_str(),
        created_at: token.record().created_at(),
        expires_at: token.record().expires_at(),
    }) {
        Ok(bytes) => response(StatusCode::CREATED, "application/json", bytes),
        Err(_) => unavailable(),
    }
}

fn invalid_request() -> Response<Full<Bytes>> {
    response(
        StatusCode::BAD_REQUEST,
        "application/json",
        b"{\"error\":\"invalid PAT request\"}".to_vec(),
    )
}

#[cfg(test)]
mod tests {
    use super::{IssueRequest, IssueResponse};

    #[test]
    fn pat_request_refuses_extra_duplicate_and_non_integer_fields() {
        let base = r#""principal_id":"00000000-0000-0000-0000-000000000001","label":"fixture""#;
        for body in [
            "{}".to_owned(),
            format!("{{{base},\"lifetime_seconds\":1,\"roles\":[\"admin\"]}}"),
            format!("{{{base},\"lifetime_seconds\":1,\"lifetime_seconds\":1}}"),
            format!("{{{base},\"lifetime_seconds\":-1}}"),
            format!("{{{base},\"lifetime_seconds\":1.5}}"),
        ] {
            assert!(serde_json::from_str::<IssueRequest>(&body).is_err());
        }
        assert!(
            serde_json::from_str::<IssueRequest>(&format!("{{{base},\"lifetime_seconds\":1}}"))
                .is_ok()
        );
    }

    #[test]
    fn pat_response_has_one_wire_shape() {
        let value = serde_json::to_value(IssueResponse {
            token: "fixture-token",
            token_prefix: "fixture-prefix",
            principal_id: "fixture-principal",
            created_at: "2026-09-10T00:00:00Z",
            expires_at: "2026-09-11T00:00:00Z",
        })
        .expect("fixture response");
        assert_eq!(
            value,
            serde_json::json!({
                "token": "fixture-token",
                "token_prefix": "fixture-prefix",
                "principal_id": "fixture-principal",
                "created_at": "2026-09-10T00:00:00Z",
                "expires_at": "2026-09-11T00:00:00Z"
            })
        );
    }
}
