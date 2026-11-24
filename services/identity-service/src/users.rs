//! Identity requests from the control service, operation workers and host pods.

use std::net::IpAddr;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::{Request, Response, StatusCode, body::Incoming};
use sqlx::PgConnection as Client;
use wamn_authentication::identity_error::{IdentityErrorType, IdentityRefusal};
use wamn_identity::identity::{
    create_or_reuse_user,
    issuer::{Issuer, begin_user_write},
};
use wamn_identity::password::RefusalCause;
use wamn_identity::password_requests::{self, Attempt, DeliveryFailure, EmailDelivery};
use wamn_identity::workload_identity::Role;
use wamn_identity_protocol::PrincipalId;
use wamn_identity_protocol::wire::{PrincipalRequest, UserRequest, UserResponse};
use wamn_registry::OrgId;

use crate::{
    Inner, connect_database,
    mail::Mailer,
    nats,
    password::{refusal, throttled},
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
    let (parts, body) = request.into_parts();
    let Some(token) = workload::bearer(&parts.headers) else {
        return forbidden();
    };
    let Some(api) = &inner.kubernetes else {
        return unavailable();
    };
    let route = parts.uri.path();
    if route == wamn_identity_protocol::nats::SIGN_HOST_USER_PATH {
        let pod = match workload::authenticate_host(api, token).await {
            Ok(Some(pod)) => pod,
            Ok(None) => return forbidden(),
            Err(error) => {
                tracing::warn!(%error, "check identity host token failed");
                return unavailable();
            }
        };
        let Some(body) = json_body(&parts.headers, body).await else {
            return invalid();
        };
        return nats::sign_host_user(inner, &pod, &body);
    }
    let nats_route = matches!(
        route,
        wamn_identity_protocol::nats::ENSURE_ACCOUNT_PATH
            | wamn_identity_protocol::nats::SIGN_USER_PATH
    );
    let role = if nats_route {
        Role::Worker
    } else {
        Role::Control
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
    match Issuer(&mut db.client).registered(&org).await {
        Ok(true) => (),
        Ok(false) => return forbidden(),
        Err(_) => return unavailable(),
    }
    if !nats_route {
        match password_requests::admit(
            &mut Issuer(&mut db.client),
            &inner.issuer,
            Attempt::Source(source),
        )
        .await
        {
            Ok(true) => (),
            Ok(false) => return throttled(),
            Err(_) => return unavailable(),
        }
    }
    let Some(bytes) = json_body(&parts.headers, body).await else {
        return invalid();
    };

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

async fn json_body(headers: &hyper::HeaderMap, body: Incoming) -> Option<Bytes> {
    let mut types = headers.get_all(hyper::header::CONTENT_TYPE).iter();
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
        return None;
    }
    Some(Limited::new(body, 8192).collect().await.ok()?.to_bytes())
}

async fn disable(
    client: &mut Client,
    org: &OrgId,
    principal: &PrincipalId,
) -> Response<Full<Bytes>> {
    let Ok(mut tx) = begin_user_write(client).await else {
        return unavailable();
    };
    if let Err(error) = wamn_identity::identity::org::disable_user(&mut tx, principal, org).await {
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
    let mut store = Issuer(client);
    let invitation = match password_requests::invite(&mut store, org, &principal).await {
        Ok(invitation) => invitation,
        Err(DeliveryFailure::UnknownPrincipal) => {
            tracing::info!(principal = %principal, cause = %RefusalCause::NoSuchAccount, "password request refused");
            return invalid();
        }
        Err(DeliveryFailure::Refused { error, email }) => return refusal(&error, &email),
        Err(DeliveryFailure::Unavailable) => return unavailable(),
    };
    let delivery = async {
        mail.invite(
            &invitation.email,
            &principal,
            invitation.token.secret(),
            invitation.token.expires_at(),
        )
        .await
        .is_ok()
    };
    if password_requests::deliver(
        &mut store,
        &invitation.token,
        EmailDelivery::Invitation,
        delivery,
    )
    .await
    {
        response(
            StatusCode::CREATED,
            "application/json",
            br#"{"status":"accepted_for_delivery"}"#.to_vec(),
        )
    } else {
        unavailable()
    }
}

/// Create or reuse the user of one email, as `wamn:identity-service`, and reply
/// with the principal and whether it has a password credential.
async fn create_user(client: &mut Client, request: &UserRequest) -> Response<Full<Bytes>> {
    let Ok(mut tx) = begin_user_write(client).await else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::password::tests::demo_members;
    use wamn_identity::identity::password::issue_invitation;
    use wamn_identity_protocol::PlatformComponent;
    use wamn_test_infrastructure::containers::start_postgres;

    #[tokio::test]
    #[ignore = "requires: docker"]
    async fn failed_delivery_consumes_only_the_new_invitation() {
        let postgres = start_postgres().await.unwrap();
        let (mut admin, url, members) = demo_members(
            &postgres,
            "https://invitation-fixture.invalid",
            &[("alice", "alice@example.invalid", "Alice")],
        )
        .await;
        let principal = *members[0].id();
        let mut issuer = connect_database(url.as_str()).await.unwrap();
        let earlier = issue_invitation(
            &mut issuer.client,
            &PlatformComponent::IdentityService.principal_id(),
            &principal,
        )
        .await
        .unwrap();
        // A full disk refuses the local mail record.
        let full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let mail = Mailer::Local(tokio::sync::Mutex::new(tokio::fs::File::from_std(full)));
        let reply = invite(
            &mut issuer.client,
            &mail,
            &"demo".parse().unwrap(),
            principal,
        )
        .await;
        assert_eq!(reply.status(), StatusCode::SERVICE_UNAVAILABLE);
        let unconsumed: Vec<bool> = sqlx::query_scalar(
            "SELECT token_hash = $1 FROM identity.password_tokens WHERE consumed_at IS NULL",
        )
        .bind(wamn_digest::bytes_sha256(earlier.secret().as_bytes()).to_string())
        .fetch_all(&mut admin.client)
        .await
        .unwrap();
        assert_eq!(unconsumed, [true]);
    }
}
