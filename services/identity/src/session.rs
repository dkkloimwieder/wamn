//! Password login uses the configured audiences and current roles.
//!
//! A later login provider must establish the same principal and evidence start.
//! It cannot supply environment roles, a database URL, or a new token profile.

use sqlx::Row as _;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode};
use ring::rand::{SecureRandom as _, SystemRandom};
use wamn_identity_protocol::token::{SessionAuthority, SessionClaims};
use wamn_platform_identity::{
    AuthenticatedPrincipal,
    control::{control_audience_org, control_projects, is_org_admin},
    has_project_env_membership,
};

use crate::{ConfiguredTarget, Inner, connect_database, response};

const REGISTERED_ORG_SQL: &str = wamn_registry::sql::IDENTITY_SRC_SESSION_REGISTERED_ORG_SQL;
// An inactive environment offers and mints no audience
// (docs/architecture/execution.md#routing-and-availability).
const CURRENT_TARGET_SQL: &str = wamn_registry::sql::IDENTITY_SRC_SESSION_CURRENT_TARGET_SQL;
const ENVIRONMENT_ROLES_SQL: &str = "SELECT r.role_name FROM platform.\"user\" u \
    JOIN platform.user_role r ON r.user_id = u.id \
    WHERE u.id = $1::text::uuid AND u.status = 'active' \
    ORDER BY r.role_name";

#[derive(Debug)]
pub(super) enum FailureType {
    Unauthorized,
    Unavailable,
}

#[derive(Debug)]
pub(super) struct ExchangeFailure {
    pub(super) type_: FailureType,
}

fn refused() -> ExchangeFailure {
    ExchangeFailure {
        type_: FailureType::Unauthorized,
    }
}
fn failed() -> ExchangeFailure {
    ExchangeFailure {
        type_: FailureType::Unavailable,
    }
}

/// The audience a password login asks for: a configured project environment, or
/// the control audience of an org
/// (docs/architecture/execution.md#control-sessions), which is derived and
/// never configured.
pub(super) enum Audience<'a> {
    Environment(&'a ConfiguredTarget),
    Control { audience: &'a str, org: &'a str },
}

/// Resolve the audience requested by a password login.
pub(super) fn audience<'a>(inner: &'a Inner, audience: &'a str) -> Option<Audience<'a>> {
    match inner.targets.get(audience) {
        Some(configured) => Some(Audience::Environment(configured)),
        None => control_audience_org(audience).map(|org| Audience::Control { audience, org }),
    }
}

/// The JWT claims of a password-login session for the resolved audience, after
/// the same current authority tests as discovery.
pub(super) async fn claims_for_audience(
    inner: &Inner,
    client: &mut sqlx::PgConnection,
    principal: &AuthenticatedPrincipal,
    audience: &Audience<'_>,
    authority: SessionAuthority,
) -> Result<SessionClaims, ExchangeFailure> {
    match *audience {
        Audience::Environment(configured) => {
            claims_for_principal(inner, principal, configured, authority).await
        }
        Audience::Control { audience, org } => {
            if !holds_control(client, principal, org).await? {
                return Err(refused());
            }
            // A control session carries no application roles.
            claims(inner, principal, org, audience, Vec::new(), authority)
        }
    }
}

/// Whether the principal holds a control role in the registered org.
pub(super) async fn holds_control(
    client: &mut sqlx::PgConnection,
    principal: &AuthenticatedPrincipal,
    org: &str,
) -> Result<bool, ExchangeFailure> {
    let registered: bool = sqlx::query(REGISTERED_ORG_SQL)
        .bind(org)
        .fetch_one(&mut *client)
        .await
        .map_err(|_| failed())?
        .get(0);
    if !registered {
        return Ok(false);
    }
    let principal_id = principal.principal().id();
    Ok(is_org_admin(client, principal_id, org)
        .await
        .map_err(|_| failed())?
        || !control_projects(client, principal_id, org)
            .await
            .map_err(|_| failed())?
            .is_empty())
}

pub(super) async fn claims_for_principal(
    inner: &Inner,
    principal: &AuthenticatedPrincipal,
    configured: &ConfiguredTarget,
    authority: SessionAuthority,
) -> Result<SessionClaims, ExchangeFailure> {
    let roles = authorized_roles(inner, principal, configured).await?;
    let target = &configured.binding;
    claims(
        inner,
        principal,
        &target.triple().org,
        target.audience(),
        roles,
        authority,
    )
}

fn claims(
    inner: &Inner,
    principal: &AuthenticatedPrincipal,
    org: &str,
    audience: &str,
    roles: Vec<String>,
    authority: SessionAuthority,
) -> Result<SessionClaims, ExchangeFailure> {
    let mut random = [0u8; 32];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| failed())?;
    let jti: String = random.iter().fold(String::new(), |mut out, byte| {
        use std::fmt::Write as _;
        write!(out, "{byte:02x}").expect("writing to a string is infallible");
        out
    });
    Ok(SessionClaims {
        iss: inner.issuer.clone(),
        sub: *principal.principal().id(),
        org: org.to_owned(),
        aud: audience.to_owned(),
        roles: roles
            .into_iter()
            .map(|role| role.parse().map_err(|_| refused()))
            .collect::<Result<_, _>>()?,
        exp: 0,
        iat: 0,
        jti,
        authority,
        csrf: None,
    })
}

// Discovery and issuance share the same current authority tests. A discovery
// result is not a credential: issuance repeats these tests after selection.
pub(super) async fn authorized_roles(
    inner: &Inner,
    principal: &AuthenticatedPrincipal,
    configured: &ConfiguredTarget,
) -> Result<Vec<String>, ExchangeFailure> {
    let principal = principal.principal();
    let target = &configured.binding;
    let triple = target.triple();
    let mut database = inner.database().await.map_err(|_| failed())?;
    if !has_project_env_membership(
        &mut database.client,
        principal.id(),
        &triple.org,
        &triple.project,
        triple.env.as_str(),
    )
    .await
    .map_err(|_| failed())?
    {
        return Err(refused());
    }
    let current: bool = sqlx::query(CURRENT_TARGET_SQL)
        .bind(&triple.org)
        .bind(&triple.project)
        .bind(triple.env.as_str())
        .bind(target.instance_suffix())
        .fetch_one(&mut database.client)
        .await
        .map_err(|_| failed())?
        .get(0);
    if !current {
        return Err(refused());
    }

    if wamn_platform_identity::control::is_project_admin(
        &mut database.client,
        principal.id(),
        &triple.org,
        &triple.project,
        Some(triple.env.as_str()),
    )
    .await
    .map_err(|_| failed())?
    {
        return Ok(vec![wamn_identity_protocol::ADMIN_ROLE.to_owned()]);
    }

    // Take the connection out while it is in use. Cancellation or a failed
    // query drops its driver, so no abandoned query stays on a retained reader.
    let mut retained = configured.reader.lock().await;
    let mut roles_database = match retained.take() {
        Some(database) => database,
        None => connect_database(&target.connection().to_json().map_err(|_| failed())?)
            .await
            .map_err(|_| failed())?,
    };
    let rows = sqlx::query(ENVIRONMENT_ROLES_SQL)
        .bind(principal.id().to_string().as_str())
        .fetch_all(&mut roles_database.client)
        .await
        .map_err(|_| failed())?;
    let roles: Vec<String> = rows.iter().map(|row| row.get(0)).collect();
    *retained = Some(roles_database);
    drop(retained);
    if roles.is_empty() {
        return Err(refused());
    }

    Ok(roles)
}

pub(super) fn unix_seconds() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs()
        .try_into()
        .ok()
}

pub(super) fn unauthorized() -> Response<Full<Bytes>> {
    let mut response = response(
        StatusCode::UNAUTHORIZED,
        "application/json",
        b"{\"error\":\"unauthorized\"}".to_vec(),
    );
    response.headers_mut().insert(
        hyper::header::WWW_AUTHENTICATE,
        hyper::header::HeaderValue::from_static("Bearer"),
    );
    response
}
