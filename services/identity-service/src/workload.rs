//! Org workload identity from the Kubernetes TokenReview API.

use hyper::{HeaderMap, header};
use wamn_identity::workload_identity::{self, Role};
use wamn_identity_client::TOKEN_AUDIENCE;
use wamn_kubernetes::client::KubernetesApi;
use wamn_registry::OrgId;

pub(super) fn bearer(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let (scheme, token) = values.next()?.to_str().ok()?.split_once(' ')?;
    (values.next().is_none()
        && scheme.eq_ignore_ascii_case("Bearer")
        && !token.is_empty()
        && !token.bytes().any(|byte| byte.is_ascii_whitespace()))
    .then_some(token)
}

pub(super) async fn authenticate(
    api: &KubernetesApi,
    token: &str,
    role: Role,
) -> Result<Option<OrgId>, wamn_kubernetes::Error> {
    let Some(account) = api
        .review_service_account_token(token, TOKEN_AUDIENCE)
        .await?
    else {
        return Ok(None);
    };
    Ok(workload_identity::org(
        account.namespace,
        &account.name,
        role,
    ))
}

pub(super) async fn authenticate_host(
    api: &KubernetesApi,
    token: &str,
) -> Result<Option<String>, wamn_kubernetes::Error> {
    let Some(account) = api
        .review_service_account_token(token, TOKEN_AUDIENCE)
        .await?
    else {
        return Ok(None);
    };
    Ok(workload_identity::host(
        account.namespace,
        &account.name,
        account.pod_name,
    ))
}
