//! Org workload identity from the Kubernetes TokenReview API.

use hyper::{HeaderMap, header};
use sqlx::PgConnection;
use wamn_identity_client::TOKEN_AUDIENCE;
use wamn_identity_protocol::PlatformComponent;
use wamn_kubernetes::client::KubernetesApi;
use wamn_registry::{EnvName, OrgId, name};

/// The service account allowed to use a protected route.
#[derive(Clone, Copy)]
pub(super) enum Role {
    Control,
    Worker,
}

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
    let Ok(org) = OrgId::try_from(account.namespace) else {
        return Ok(None);
    };
    let allowed = match role {
        Role::Control => name::control_service_account(&org),
        Role::Worker => name::operation(&org),
    };
    Ok((account.name == allowed).then_some(org))
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
    let Ok(org) = OrgId::try_from(account.namespace) else {
        return Ok(None);
    };
    let Some(env) = account
        .name
        .rsplit_once("--")
        .and_then(|(_, env)| env.parse::<EnvName>().ok())
    else {
        return Ok(None);
    };
    if account.name != name::environment_service_account(&org, &env) {
        return Ok(None);
    }
    // The pod ID must fit a single wash-runtime host subject token.
    Ok(account.pod_name.filter(|pod| {
        !pod.is_empty()
            && pod.len() <= 128
            && pod
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    }))
}

pub(super) async fn registered(
    client: &mut PgConnection,
    org: &OrgId,
) -> Result<bool, sqlx::Error> {
    let actor = PlatformComponent::IdentityService.principal_id();
    let mut tx = wamn_postgres::identity::transaction::read_connection(client, &actor).await?;
    let exists = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM registry.orgs WHERE id = $1)")
        .bind(org.as_str())
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(exists)
}
