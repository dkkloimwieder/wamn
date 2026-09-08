//! PAT presentation stays separate from canonical-principal session minting.
//!
//! A later login provider must establish the same principal and evidence start.
//! It cannot supply environment roles, a database URL, or a new token profile.

use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::{Request, Response, StatusCode, body::Incoming};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::{Deserialize, Serialize};
use wamn_platform_identity::{
    AuthenticatedPrincipal, PrincipalKind, authenticate_pat, has_project_env_membership,
    session_token::{IssuedSessionToken, SessionClaims, sign_session_token},
};

use crate::{ConfiguredTarget, IO_TIMEOUT, Inner, connect_database, response, unavailable};

// The request contains one bounded provisioned audience, never user documents.
const MAX_REQUEST_BYTES: usize = 1024;
const CURRENT_TARGET_SQL: &str = "SELECT EXISTS (SELECT 1 FROM registry.project_envs \
    WHERE org = $1 AND project = $2 AND env = $3 AND instance_suffix = $4)";
const ENVIRONMENT_ROLES_SQL: &str = "SELECT r.role_name FROM app_system.users u \
    JOIN app_system.user_roles r ON r.tenant_id = u.tenant_id AND r.user_id = u.id \
    WHERE u.tenant_id = $1 AND u.id = $2::text::uuid AND u.status = 'active' \
    ORDER BY r.role_name";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExchangeRequest {
    aud: String,
}

#[derive(Serialize)]
struct ExchangeResponse<'a> {
    access_token: &'a str,
    token_type: &'static str,
    expires_at: i64,
}

#[derive(Debug)]
enum FailureKind {
    Unauthorized,
    Unavailable,
}

#[derive(Debug)]
struct ExchangeFailure {
    kind: FailureKind,
}

fn refused() -> ExchangeFailure {
    ExchangeFailure {
        kind: FailureKind::Unauthorized,
    }
}
fn failed() -> ExchangeFailure {
    ExchangeFailure {
        kind: FailureKind::Unavailable,
    }
}

pub(super) async fn respond(inner: &Inner, request: Request<Incoming>) -> Response<Full<Bytes>> {
    // Include body delivery and every authoritative read in the original age.
    let Some(started_at) = unix_seconds() else {
        return unavailable();
    };
    match tokio::time::timeout(IO_TIMEOUT, exchange(inner, request, started_at)).await {
        Ok(Ok(token)) => token_response(&token),
        Ok(Err(ExchangeFailure {
            kind: FailureKind::Unauthorized,
        })) => unauthorized(),
        Ok(Err(ExchangeFailure {
            kind: FailureKind::Unavailable,
        }))
        | Err(_) => unavailable(),
    }
}

async fn exchange(
    inner: &Inner,
    request: Request<Incoming>,
    started_at: i64,
) -> Result<IssuedSessionToken, ExchangeFailure> {
    let (parts, body) = request.into_parts();
    let mut authorization = parts.headers.get_all(hyper::header::AUTHORIZATION).iter();
    let value = authorization
        .next()
        .ok_or_else(refused)?
        .to_str()
        .map_err(|_| refused())?;
    if authorization.next().is_some() {
        return Err(refused());
    }
    let (scheme, pat) = value.split_once(' ').ok_or_else(refused)?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return Err(refused());
    }
    let mut content_types = parts.headers.get_all(hyper::header::CONTENT_TYPE).iter();
    let content_type = content_types
        .next()
        .ok_or_else(refused)?
        .to_str()
        .map_err(|_| refused())?;
    if content_types.next().is_some()
        || !content_type
            .split(';')
            .next()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
    {
        return Err(refused());
    }
    let bytes = Limited::new(body, MAX_REQUEST_BYTES)
        .collect()
        .await
        .map_err(|_| refused())?
        .to_bytes();
    let request: ExchangeRequest = serde_json::from_slice(&bytes).map_err(|_| refused())?;
    let target = inner.targets.get(&request.aud).ok_or_else(refused)?;
    let principal = authenticate_pat(&inner.database.client, pat)
        .await
        .map_err(|_| failed())?
        .ok_or_else(refused)?;
    if principal.principal().kind() != PrincipalKind::Human {
        return Err(refused());
    }
    mint_for_principal(inner, &principal, target, started_at).await
}

// This is an internal seam, not an unauthenticated principal-to-token API.
// Its caller must establish the principal using the current credential path.
async fn mint_for_principal(
    inner: &Inner,
    principal: &AuthenticatedPrincipal,
    configured: &ConfiguredTarget,
    started_at: i64,
) -> Result<IssuedSessionToken, ExchangeFailure> {
    let principal = principal.principal();
    let target = &configured.binding;
    let triple = target.triple();
    if !has_project_env_membership(
        &inner.database.client,
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
    let current: bool = inner
        .database
        .client
        .query_one(
            CURRENT_TARGET_SQL,
            &[
                &triple.org,
                &triple.project,
                &triple.env.as_str(),
                &target.instance_suffix(),
            ],
        )
        .await
        .map_err(|_| failed())?
        .get(0);
    if !current {
        return Err(refused());
    }

    // Take the connection out while it is in use. Cancellation or a failed
    // query drops its driver, so no abandoned query stays on a retained reader.
    let mut retained = configured.reader.lock().await;
    let roles_database = match retained.take().filter(|db| !db.client.is_closed()) {
        Some(database) => database,
        None => connect_database(target.connection().url())
            .await
            .map_err(|_| failed())?,
    };
    let rows = roles_database
        .client
        .query(
            ENVIRONMENT_ROLES_SQL,
            &[&target.tenant_id(), &principal.id().as_str()],
        )
        .await
        .map_err(|_| failed())?;
    let roles: Vec<String> = rows.iter().map(|row| row.get(0)).collect();
    *retained = Some(roles_database);
    drop(retained);
    if roles.is_empty() {
        return Err(refused());
    }

    let mut random = [0u8; 32];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| failed())?;
    let jti: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let claims = SessionClaims {
        iss: inner.issuer.clone(),
        sub: principal.id().to_string(),
        org: triple.org.clone(),
        aud: target.audience().to_owned(),
        roles,
        exp: 0,
        iat: 0,
        jti,
    };
    let mut signing = inner.signing.as_ref().ok_or_else(failed)?.lock().await;
    sign_session_token(&mut signing.client, claims, started_at)
        .await
        .map_err(|_| failed())
}

fn unix_seconds() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs()
        .try_into()
        .ok()
}

fn token_response(token: &IssuedSessionToken) -> Response<Full<Bytes>> {
    match serde_json::to_vec(&ExchangeResponse {
        access_token: token.token(),
        token_type: "Bearer",
        expires_at: token.claims().exp,
    }) {
        Ok(bytes) => response(StatusCode::OK, "application/json", bytes),
        Err(_) => unavailable(),
    }
}

fn unauthorized() -> Response<Full<Bytes>> {
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

#[cfg(test)]
mod tests {
    use super::{ExchangeRequest, ExchangeResponse};

    #[test]
    fn session_response_has_one_frozen_wire_shape() {
        let bytes = serde_json::to_vec(&ExchangeResponse {
            access_token: "fixture-token",
            token_type: "Bearer",
            expires_at: 1234,
        })
        .unwrap();
        assert_eq!(
            bytes,
            br#"{"access_token":"fixture-token","token_type":"Bearer","expires_at":1234}"#
        );
    }

    #[test]
    fn session_request_cannot_supply_authority_or_duplicate_an_audience() {
        for bytes in [
            br#"{"aud":"a","aud":"b"}"#.as_slice(),
            br#"{"aud":"a","org":"other"}"#,
            br#"{"aud":"a","database_url":"postgres://untrusted/db"}"#,
            br#"{"aud":"a","roles":["admin"]}"#,
        ] {
            assert!(serde_json::from_slice::<ExchangeRequest>(bytes).is_err());
        }
    }
}
