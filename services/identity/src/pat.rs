//! Only a verified provisioning-operator certificate authorizes PAT issuance.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::{Request, Response, StatusCode, body::Incoming};
use wamn_control_provision::{PlatformComponent, bind_platform_principal_sql};
use wamn_identity_protocol::PrincipalId;
use wamn_identity_protocol::wire::{IssuePatRequest as IssueRequest, PatResponse as IssueResponse};
use wamn_platform_identity::{IdentityErrorType, IssuedPat, issue_pat};

use crate::{IO_TIMEOUT, Inner, response, unavailable};

// One principal ID, a library-bounded label, and an integer lifetime fit here.
const MAX_REQUEST_BYTES: usize = 1024;

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
    let principal = request.principal_id;
    // Issuance has no user caller yet, so the token row stamps wamn:provisioning.
    let mut issuance = inner.issuance.lock().await;
    if inner.reopen(&mut issuance).await.is_err() {
        return unavailable();
    }
    let Ok(transaction) = issuance.client.transaction().await else {
        return unavailable();
    };
    if transaction
        .batch_execute(&bind_platform_principal_sql(
            PlatformComponent::Provisioning,
        ))
        .await
        .is_err()
    {
        return unavailable();
    }
    let token = match issue_pat(
        &transaction,
        &principal,
        &request.label,
        Duration::from_secs(request.lifetime_seconds),
    )
    .await
    {
        Ok(token) => token,
        Err(error) => {
            return match error.error_type() {
                IdentityErrorType::InvalidInput | IdentityErrorType::NotFound => invalid_request(),
                _ => unavailable(),
            };
        }
    };
    if transaction.commit().await.is_err() {
        return unavailable();
    }
    token_response(&token, &principal)
}

fn token_response(token: &IssuedPat, principal: &PrincipalId) -> Response<Full<Bytes>> {
    match serde_json::to_vec(&IssueResponse {
        token: token.token(),
        token_prefix: token.record().prefix(),
        principal_id: *principal,
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
    use super::IssueRequest;

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
}
