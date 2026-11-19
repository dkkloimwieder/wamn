//! Identity requests from the control service or the operation worker of one org.

use std::net::IpAddr;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::{Request, Response, StatusCode, body::Incoming};
use sqlx::{Connection as _, PgConnection as Client};
use wamn_identity::password::{RefusalCause, issue_invitation};
use wamn_identity::{IdentityErrorType, IdentityRefusal, create_or_reuse_user};
use wamn_identity_protocol::PrincipalId;
use wamn_identity_protocol::wire::{PrincipalRequest, UserRequest, UserResponse};
use wamn_provision::PlatformComponent;
use wamn_registry::OrgId;

use crate::{
    Inner, connect_database,
    mail::Mailer,
    nats,
    password::{admit_source, refusal, throttled},
    response, unavailable, workload,
};

pub(super) async fn respond(
    inner: &Inner,
    request: Request<Incoming>,
    source: IpAddr,
) -> Response<Full<Bytes>> {
    let Ok(_permit) = inner.request_slots.try_acquire() else {
        return throttled();
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        handle(inner, request, source),
    )
    .await
    .unwrap_or_else(|_| unavailable())
}

async fn handle(
    inner: &Inner,
    request: Request<Incoming>,
    source: IpAddr,
) -> Response<Full<Bytes>> {
    let Some(token) = workload::bearer(request.headers()) else {
        return forbidden();
    };
    let Some(api) = &inner.kubernetes else {
        return unavailable();
    };
    let nats_route = matches!(
        request.uri().path(),
        wamn_identity_client::nats::ENSURE_ACCOUNT_PATH
            | wamn_identity_client::nats::SIGN_USER_PATH
    );
    let role = if nats_route {
        workload::Role::Worker
    } else {
        workload::Role::Control
    };
    let org = match workload::authenticate(api, token, role).await {
        Ok(Some(org)) => org,
        Ok(None) => return forbidden(),
        Err(error) => {
            tracing::warn!(%error, "check identity workload token failed");
            return unavailable();
        }
    };
    let Ok(mut db) = connect_database(&inner.connection).await else {
        return unavailable();
    };
    match workload::registered(&mut db.client, &org).await {
        Ok(true) => (),
        Ok(false) => return forbidden(),
        Err(_) => return unavailable(),
    }
    if !nats_route {
        match admit_source(&mut db.client, &inner.issuer, source).await {
            Ok(true) => (),
            Ok(false) => return throttled(),
            Err(_) => return unavailable(),
        }
    }
    let (parts, body) = request.into_parts();
    let mut types = parts.headers.get_all(hyper::header::CONTENT_TYPE).iter();
    if !types
        .next()
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
        })
        || types.next().is_some()
    {
        return invalid();
    }
    let Ok(body) = Limited::new(body, 8192).collect().await else {
        return invalid();
    };
    let bytes = body.to_bytes();

    let route = parts.uri.path();
    if nats_route {
        return nats::respond(inner, &mut db.client, &org, route, &bytes).await;
    }
    if route == "/users" {
        let Ok(request) = serde_json::from_slice::<UserRequest>(&bytes) else {
            return invalid();
        };
        return create_user(&mut db.client, &request).await;
    }

    let Ok(request) = serde_json::from_slice::<PrincipalRequest>(&bytes) else {
        return invalid();
    };
    if route == "/invitations" {
        let Some(state) = &inner.passwords else {
            return response(StatusCode::NOT_FOUND, "text/plain", b"not found\n".to_vec());
        };
        return invite(&mut db.client, &state.mail, &org, request.principal_id).await;
    }

    disable(&mut db.client, &org, &request.principal_id).await
}

async fn begin(client: &mut Client) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, sqlx::Error> {
    let mut tx = client
        .begin_with("BEGIN ISOLATION LEVEL READ COMMITTED")
        .await?;
    sqlx::query(
        "INSERT INTO platform.tx_identity(principal_id,role,application,operation)
         VALUES($1,'','identity','wamn:provisioning')",
    )
    .bind(PlatformComponent::Provisioning.principal_id())
    .execute(&mut *tx)
    .await?;
    Ok(tx)
}

async fn disable(
    client: &mut Client,
    org: &OrgId,
    principal: &PrincipalId,
) -> Response<Full<Bytes>> {
    let Ok(mut tx) = begin(client).await else {
        return unavailable();
    };
    if let Err(error) = wamn_identity::org::disable_user(&mut tx, principal, org).await {
        return match error.refusal() {
            Some(IdentityRefusal::UserNotFound) => response(
                StatusCode::NOT_FOUND,
                "application/json",
                br#"{"error":"user_not_found"}"#.to_vec(),
            ),
            Some(IdentityRefusal::UserInAnotherOrg) => response(
                StatusCode::CONFLICT,
                "application/json",
                br#"{"error":"user_in_another_org"}"#.to_vec(),
            ),
            _ => unavailable(),
        };
    }
    if tx.commit().await.is_err() {
        return unavailable();
    }
    response(StatusCode::NO_CONTENT, "application/json", Vec::new())
}

async fn invite(
    client: &mut Client,
    mail: &Mailer,
    org: &OrgId,
    principal: PrincipalId,
) -> Response<Full<Bytes>> {
    let Ok(mut read) = wamn_identity::transaction::read_connection(
        client,
        &PlatformComponent::Provisioning.principal_id(),
    )
    .await
    else {
        return unavailable();
    };
    let Ok(email) = sqlx::query_scalar::<_, String>(
        "SELECT p.email FROM identity.principals p
         JOIN identity.org_memberships m ON m.principal_id = p.id
         WHERE p.id = $1 AND p.type = 'user' AND p.status = 'active'
               AND m.org = $2 AND m.status = 'active'",
    )
    .bind(principal)
    .bind(org.as_str())
    .fetch_optional(&mut *read)
    .await
    else {
        return unavailable();
    };
    if read.commit().await.is_err() {
        return unavailable();
    }
    let Some(email) = email else {
        tracing::info!(principal = %principal, cause = %RefusalCause::NoSuchAccount, "password request refused");
        return invalid();
    };
    let actor: PrincipalId = PlatformComponent::Provisioning.principal_id();
    let invitation = match issue_invitation(client, &actor, &principal).await {
        Ok(value) => value,
        Err(error) => return refusal(&error, &email),
    };
    if mail
        .invite(
            &email,
            &principal,
            invitation.secret(),
            invitation.expires_at(),
        )
        .await
        .is_err()
    {
        // A failed or uncertain send must never report delivery success.
        // Consume only this invitation. Earlier invitations stay valid.
        let hash = wamn_digest::bytes_sha256(invitation.secret().as_bytes());
        if let Ok(mut tx) = begin(client).await
            && sqlx::query(
                "UPDATE identity.password_tokens SET consumed_at = clock_timestamp()
                 WHERE token_hash = $1 AND consumed_at IS NULL",
            )
            .bind(hash)
            .execute(&mut *tx)
            .await
            .is_ok()
        {
            let _ = tx.commit().await;
        }
        return unavailable();
    }
    response(
        StatusCode::CREATED,
        "application/json",
        br#"{"status":"accepted_for_delivery"}"#.to_vec(),
    )
}

/// Create or reuse the user of one email, as `wamn:provisioning`, and reply
/// with the principal and whether it has a password credential.
async fn create_user(client: &mut Client, request: &UserRequest) -> Response<Full<Bytes>> {
    let Ok(mut tx) = begin(client).await else {
        return unavailable();
    };
    let account = match create_or_reuse_user(&mut tx, &request.email, &request.display_name).await {
        Ok(account) => account,
        Err(error) => {
            let status = match error.error_type() {
                IdentityErrorType::InvalidInput => StatusCode::BAD_REQUEST,
                IdentityErrorType::Conflict => StatusCode::CONFLICT,
                _ => return unavailable(),
            };
            let body = serde_json::json!({ "error": error.to_string() });
            return response(status, "application/json", body.to_string().into_bytes());
        }
    };
    if tx.commit().await.is_err() {
        return unavailable();
    }
    let body = UserResponse {
        principal_id: account.principal_id,
        enrolled: account.enrolled,
    };
    response(
        StatusCode::OK,
        "application/json",
        serde_json::to_vec(&body).expect("user response"),
    )
}

pub(super) fn forbidden() -> Response<Full<Bytes>> {
    response(
        StatusCode::FORBIDDEN,
        "application/json",
        br#"{"error":"workload identity refused"}"#.to_vec(),
    )
}

fn invalid() -> Response<Full<Bytes>> {
    response(
        StatusCode::BAD_REQUEST,
        "application/json",
        br#"{"error":"invalid user management request"}"#.to_vec(),
    )
}
