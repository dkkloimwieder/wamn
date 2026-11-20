//! The org invitation Job's private control operation.

use anyhow::Context as _;
use http_body_util::BodyExt as _;
use hyper::{Method, Request, StatusCode, header};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use wamn_kubernetes::client::KubernetesApi;
use wamn_platform_install::provision_org::{
    CONTROL_AUDIENCE, CONTROL_TOKEN_PATH, FIRST_OWNER_PATH, INVITATION_ACCOUNT,
};
use wamn_registry::OrgId;

use crate::host_route::HostRouteHandlers;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Invitation<'a> {
    email: &'a str,
}

pub(crate) async fn respond(
    handlers: &HostRouteHandlers,
    api: Option<&KubernetesApi>,
    request: Request<wasmtime_wasi_http::WasiBody>,
) -> StatusCode {
    if request.method() != Method::POST {
        return StatusCode::METHOD_NOT_ALLOWED;
    }
    let Some((api, token)) = api.zip(
        request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer ")),
    ) else {
        return StatusCode::UNAUTHORIZED;
    };
    match api
        .review_service_account_token(token, CONTROL_AUDIENCE)
        .await
    {
        Ok(Some(account))
            if account.namespace == handlers.org && account.name == INVITATION_ACCOUNT => {}
        Ok(_) => return StatusCode::FORBIDDEN,
        Err(error) => {
            tracing::warn!(%error, "review the org invitation Job's token");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    }

    let Ok(body) = http_body_util::Limited::new(request.into_body(), 4096)
        .collect()
        .await
    else {
        return StatusCode::BAD_REQUEST;
    };
    let body = body.to_bytes();
    let Ok(invitation) = serde_json::from_slice::<Invitation<'_>>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    match crate::control_route::users::first_owner(handlers, invitation.email).await {
        Ok(status) => status,
        Err(error) => {
            tracing::warn!(%error, "provision the first org owner");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

pub async fn invite(org: &OrgId, email: &str) -> anyhow::Result<()> {
    use wamn_http_client::{HttpClient, Redirects};

    let token = tokio::fs::read_to_string(CONTROL_TOKEN_PATH)
        .await
        .context("read the projected control token")?;
    let endpoint = format!("http://c-{org}-http.{org}.svc.cluster.local{FIRST_OWNER_PATH}");
    let client = HttpClient::builder()
        .timeout(Duration::from_secs(30))
        .redirects(Redirects::None)
        .build()?;
    let response = client
        .post(endpoint)
        .bearer_auth(token.trim())
        .json(&Invitation { email })
        .send()
        .await
        .context("contact org control for the owner invitation")?;
    anyhow::ensure!(
        response.status() == StatusCode::NO_CONTENT,
        "org control returned {} for the owner invitation",
        response.status()
    );
    Ok(())
}
