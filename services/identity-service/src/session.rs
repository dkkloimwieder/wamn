//! Password login issues one access token for an active org member.

use crate::{Inner, response};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode};
use ring::rand::{SecureRandom as _, SystemRandom};
use sqlx::Row as _;
use std::time::{SystemTime, UNIX_EPOCH};
use wamn_identity::{
    AuthenticatedPrincipal, control::org_audience_org, session_token::AccessTokenClaims,
};

#[derive(Debug)]
pub(super) enum FailureType {
    Unauthorized,
    Unavailable,
}

#[derive(Debug)]
pub(super) struct ExchangeFailure {
    pub(super) type_: FailureType,
}

fn failed() -> ExchangeFailure {
    ExchangeFailure {
        type_: FailureType::Unavailable,
    }
}

pub(super) async fn claims_for_audience(
    inner: &Inner,
    client: &mut sqlx::PgConnection,
    principal: &AuthenticatedPrincipal,
    audience: &str,
) -> Result<AccessTokenClaims, ExchangeFailure> {
    let org = org_audience_org(audience).ok_or(ExchangeFailure {
        type_: FailureType::Unauthorized,
    })?;
    let member: bool = sqlx::query("SELECT EXISTS (SELECT 1 FROM identity.org_memberships m JOIN identity.principals p ON p.id=m.principal_id WHERE m.principal_id=$1 AND m.org=$2 AND m.status='active' AND p.type='user' AND p.status='active')")
        .bind(sqlx::types::Uuid::from(*principal.principal().id())).bind(&org).fetch_one(&mut *client).await.map_err(|_| failed())?.get(0);
    if !member {
        return Err(ExchangeFailure {
            type_: FailureType::Unauthorized,
        });
    }
    let mut random = [0u8; 32];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| failed())?;
    Ok(AccessTokenClaims {
        iss: inner.issuer.clone(),
        sub: *principal.principal().id(),
        aud: audience.to_owned(),
        exp: 0,
        iat: 0,
        jti: hex::encode(random),
        client_id: "wamn-web".into(),
    })
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
