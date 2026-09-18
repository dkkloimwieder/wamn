//! Password endpoints share bounded work, database throttling, and session authority.

use std::net::IpAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::{Request, Response, StatusCode, body::Incoming};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::sync::Semaphore;
use tokio_postgres::Client;
use wamn_control_provision::{PlatformComponent, identity_issuer::IdentityIssuerConnection};
use wamn_platform_identity::{
    PrincipalId,
    password::{
        Password, PasswordError, PasswordErrorKind, PasswordWork, authenticate_password,
        enroll_password, issue_invitation, issue_reset, password_work, reset_password,
    },
    password_login,
    session_token::{IssuedSessionToken, sign_session_token_in_transaction},
};
use zeroize::Zeroizing;

use crate::{
    IdentityServiceError, Inner, connect_database,
    mail::{Mailer, ResendConfig},
    response, session, unavailable,
};

// At most eight password requests and two hashing jobs per replica, with no queue.
// Database windows cap the whole issuer at 120/minute, source at 20, account at 5.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REQUEST_BYTES: usize = 8192;

#[derive(Debug)]
pub(super) struct State {
    mail: Mailer,
    connection: IdentityIssuerConnection,
    work: PasswordWork,
    requests: Semaphore,
}
impl State {
    pub(super) fn new(
        mail: ResendConfig,
        connection: IdentityIssuerConnection,
    ) -> Result<Self, IdentityServiceError> {
        Ok(Self {
            mail: Mailer::new(mail)?,
            connection,
            work: password_work(),
            requests: Semaphore::new(8),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvitationRequest {
    principal_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollmentRequest {
    principal_id: String,
    invitation: String,
    password: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentsRequest {
    email: String,
    password: String,
}

#[derive(Serialize)]
struct Environment<'a> {
    aud: &'a str,
    org: &'a str,
    project: &'a str,
    env: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    email: String,
    password: String,
    aud: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenewalRequest {
    renewal_token: String,
    aud: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryRequest {
    email: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResetRequest {
    email: String,
    secret: String,
    password: String,
}

#[derive(Serialize)]
struct RenewableResponse<'a> {
    access_token: &'a str,
    token_type: &'static str,
    expires_at: i64,
    renewal_token: &'a str,
    login_expires_at: i64,
}

pub(super) async fn respond(
    inner: &Inner,
    state: &State,
    request: Request<Incoming>,
    operator: bool,
    source: IpAddr,
) -> Response<Full<Bytes>> {
    if request.uri().path() == "/invitations" && !operator {
        return response(
            StatusCode::FORBIDDEN,
            "application/json",
            br#"{"error":"operator certificate required"}"#.to_vec(),
        );
    }
    let Ok(_permit) = state.requests.try_acquire() else {
        return throttled();
    };
    let Some(started_at) = session::unix_seconds() else {
        return unavailable();
    };
    match tokio::time::timeout(
        REQUEST_TIMEOUT,
        handle(inner, state, request, source, started_at),
    )
    .await
    {
        Ok(reply) => reply,
        Err(_) => unavailable(),
    }
}

async fn handle(
    inner: &Inner,
    state: &State,
    request: Request<Incoming>,
    source: IpAddr,
    started_at: i64,
) -> Response<Full<Bytes>> {
    // Each admitted request owns its connection. Cancellation closes outstanding DB work.
    let Ok(mut database) = connect_database(state.connection.url()).await else {
        return unavailable();
    };
    let source = match source {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or(source, IpAddr::V4),
        IpAddr::V4(_) => source,
    };
    let source_key = format!("source:{source}");
    match admit(
        &mut database.client,
        &[("global", 120), (&source_key, 20)],
        &inner.issuer,
    )
    .await
    {
        Ok(true) => (),
        Ok(false) => return throttled(),
        Err(_) => return unavailable(),
    }
    let (parts, body) = request.into_parts();
    let mut types = parts.headers.get_all(hyper::header::CONTENT_TYPE).iter();
    if !types.next().and_then(|v| v.to_str().ok()).is_some_and(|v| {
        v.split(';')
            .next()
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))
    }) || types.next().is_some()
    {
        return invalid();
    }
    let Ok(body) = Limited::new(body, MAX_REQUEST_BYTES).collect().await else {
        return invalid();
    };
    let bytes = Zeroizing::new(body.to_bytes().to_vec());
    match parts.uri.path() {
        "/invitations" => {
            let Ok(request) = serde_json::from_slice::<InvitationRequest>(&bytes) else {
                return invalid();
            };
            let Ok(principal) = request.principal_id.parse::<PrincipalId>() else {
                return invalid();
            };
            let Ok(row) = database.client.query_opt("SELECT email FROM identity.principals WHERE id = $1::text::uuid AND kind = 'human' AND status = 'active'", &[&principal.as_str()]).await else { return unavailable(); };
            let Some(row) = row else {
                return invalid();
            };
            let email: String = row.get(0);
            let actor: PrincipalId = PlatformComponent::Provisioning
                .principal_id()
                .to_string()
                .parse()
                .expect("platform principal");
            let invitation = match issue_invitation(&mut database.client, &actor, &principal).await
            {
                Ok(value) => value,
                Err(error) => return password_failure(&error),
            };
            if state
                .mail
                .invite(&email, principal.as_str(), invitation.secret())
                .await
                .is_err()
            {
                // A failed or uncertain send must never report delivery success.
                // Consume only this invitation; earlier operator invitations stay valid.
                let hash = Sha256::digest(invitation.secret().as_bytes()).to_vec();
                if let Ok(tx) = database.client.transaction().await {
                    let _ = tx
                        .execute(
                            "SELECT set_config('app.user_id', $1, true)",
                            &[&actor.as_str()],
                        )
                        .await;
                    if tx.execute("UPDATE identity.password_tokens SET consumed_at = clock_timestamp() WHERE token_hash = $1 AND consumed_at IS NULL", &[&hash]).await.is_ok() { let _ = tx.commit().await; }
                }
                return unavailable();
            }
            response(
                StatusCode::CREATED,
                "application/json",
                br#"{"status":"accepted_for_delivery"}"#.to_vec(),
            )
        }
        "/password/enroll" => {
            let Ok(request) = serde_json::from_slice::<EnrollmentRequest>(&bytes) else {
                return invalid();
            };
            let secret = Zeroizing::new(request.invitation);
            let password = match Password::new(request.password) {
                Ok(value) => value,
                Err(error) => return password_failure(&error),
            };
            let Ok(principal) = request.principal_id.parse::<PrincipalId>() else {
                return invalid();
            };
            let bucket = format!("enroll:{principal}");
            match admit(&mut database.client, &[(&bucket, 5)], &inner.issuer).await {
                Ok(true) => (),
                Ok(false) => return throttled(),
                Err(_) => return unavailable(),
            }
            match enroll_password(
                &mut database.client,
                &state.work,
                &principal,
                &secret,
                password,
            )
            .await
            {
                Ok(()) => response(StatusCode::NO_CONTENT, "application/json", Vec::new()),
                Err(error) => password_failure(&error),
            }
        }
        "/password/session" | "/password/environments" => {
            let (email, password, audience) = if parts.uri.path() == "/password/environments" {
                let Ok(request) = serde_json::from_slice::<EnvironmentsRequest>(&bytes) else {
                    return invalid();
                };
                (request.email, request.password, None)
            } else {
                let Ok(request) = serde_json::from_slice::<LoginRequest>(&bytes) else {
                    return invalid();
                };
                (request.email, request.password, Some(request.aud))
            };
            let Ok(password) = Password::new(password) else {
                return session::unauthorized();
            };
            if email.len() > 320 {
                return session::unauthorized();
            }
            let email = email.trim().to_lowercase();
            let bucket = format!("login:{}", hex::encode(Sha256::digest(email.as_bytes())));
            match admit(&mut database.client, &[(&bucket, 5)], &inner.issuer).await {
                Ok(true) => (),
                Ok(false) => return throttled(),
                Err(_) => return unavailable(),
            }
            let Ok(tx) = database.client.transaction().await else {
                return unavailable();
            };
            let Ok(row) = tx
                .query_opt(
                    "SELECT id::text FROM identity.principals WHERE email=$1 AND kind='human'",
                    &[&email],
                )
                .await
            else {
                return unavailable();
            };
            if let Some(row) = row {
                let Ok(id) = row.get::<_, String>(0).parse::<PrincipalId>() else {
                    return unavailable();
                };
                if password_login::lock_principal(&tx, &id).await.is_err() {
                    return unavailable();
                }
            }
            let principal = match authenticate_password(&tx, &state.work, &email, password).await {
                Ok(Some(value)) => value,
                Ok(None) => return session::unauthorized(),
                Err(error) => return password_failure(&error),
            };
            let Some(audience) = audience else {
                let mut environments = Vec::new();
                for configured in inner.targets.values() {
                    match session::authorized_roles(inner, &principal, configured).await {
                        Ok(_) => {
                            let target = &configured.binding;
                            let triple = target.triple();
                            environments.push(Environment {
                                aud: target.audience(),
                                org: &triple.org,
                                project: &triple.project,
                                env: triple.env.as_str(),
                            });
                        }
                        Err(error) => match error.kind {
                            session::FailureKind::Unauthorized => (),
                            session::FailureKind::Unavailable => return unavailable(),
                        },
                    }
                }
                return match serde_json::to_vec(&serde_json::json!({"environments": environments}))
                {
                    Ok(bytes) => response(StatusCode::OK, "application/json", bytes),
                    Err(_) => unavailable(),
                };
            };
            let Some(target) = inner.targets.get(&audience) else {
                return session::unauthorized();
            };
            if tx
                .execute(
                    "SELECT set_config('app.user_id',$1,true)",
                    &[&principal.principal().id().as_str()],
                )
                .await
                .is_err()
            {
                return unavailable();
            }
            let claims = match session::claims_for_principal(inner, &principal, target).await {
                Ok(claims) => claims,
                Err(error) => return authority_failure(&error),
            };
            let renewal = match password_login::create_login(
                &tx,
                principal.principal().id(),
                &inner.issuer,
                &audience,
            )
            .await
            {
                Ok(Some(renewal)) => renewal,
                Ok(None) => return session::unauthorized(),
                Err(_) => return unavailable(),
            };
            finish_session(tx, claims, renewal, started_at).await
        }
        "/password/recover" | "/password/reset" => {
            let resetting = parts.uri.path() == "/password/reset";
            let (email, reset) = if resetting {
                let Ok(request) = serde_json::from_slice::<ResetRequest>(&bytes) else {
                    return invalid();
                };
                (
                    request.email,
                    Some((Zeroizing::new(request.secret), request.password)),
                )
            } else {
                let Ok(request) = serde_json::from_slice::<RecoveryRequest>(&bytes) else {
                    return invalid();
                };
                (request.email, None)
            };
            if email.len() > 320 {
                return invalid();
            }
            let email = email.trim().to_lowercase();
            let bucket = format!("recovery:{}", hex::encode(Sha256::digest(email.as_bytes())));
            match admit(&mut database.client, &[(&bucket, 5)], &inner.issuer).await {
                Ok(true) => (),
                Ok(false) => return throttled(),
                Err(_) => return unavailable(),
            }
            // Pad the whole account-dependent recovery path, including provider failures.
            // No mail task outlives the request or performs automatic retries.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
            let action = async {
                let Ok(row) = database.client.query_opt("SELECT p.id::text FROM identity.principals p JOIN identity.password_credentials c ON c.principal_id=p.id WHERE p.email=$1 AND p.kind='human' AND p.status='active'", &[&email]).await else { return unavailable(); };
                let Some(row) = row else {
                    return invalid();
                };
                let Ok(principal) = row.get::<_, String>(0).parse::<PrincipalId>() else {
                    return unavailable();
                };
                if let Some((secret, password)) = reset {
                    let Ok(password) = Password::new(password) else {
                        return invalid();
                    };
                    if let Err(error) = reset_password(
                        &mut database.client,
                        &state.work,
                        &principal,
                        &secret,
                        password,
                    )
                    .await
                    {
                        return password_failure(&error);
                    }
                    let notified = state.mail.password_changed(&email).await.is_ok();
                    return response(StatusCode::OK, "application/json", serde_json::to_vec(&serde_json::json!({"status":"password_reset", "notification": if notified { "accepted_for_delivery" } else { "unavailable" }})).expect("fixed response"));
                }
                let actor: PrincipalId = PlatformComponent::Provisioning
                    .principal_id()
                    .to_string()
                    .parse()
                    .expect("platform principal");
                let Ok(token) = issue_reset(&mut database.client, &actor, &principal).await else {
                    return unavailable();
                };
                if state.mail.reset(&email, token.secret()).await.is_err() {
                    let hash = Sha256::digest(token.secret().as_bytes()).to_vec();
                    if let Ok(tx) = database.client.transaction().await
                        && tx.execute("SELECT set_config('app.user_id',$1,true)", &[&actor.as_str()]).await.is_ok()
                        && tx.execute("UPDATE identity.password_tokens SET consumed_at=clock_timestamp() WHERE token_hash=$1 AND consumed_at IS NULL", &[&hash]).await.is_ok() { let _ = tx.commit().await; }
                }
                response(StatusCode::ACCEPTED, "application/json", Vec::new())
            };
            if resetting {
                return action.await;
            }
            let _ = tokio::time::timeout_at(deadline, action).await;
            tokio::time::sleep_until(deadline).await;
            response(
                StatusCode::ACCEPTED,
                "application/json",
                br#"{"status":"if_eligible_email_will_arrive"}"#.to_vec(),
            )
        }
        "/password/renew" | "/password/logout" | "/password/logout-all" => {
            let Ok(request) = serde_json::from_slice::<RenewalRequest>(&bytes) else {
                return invalid();
            };
            let secret = Zeroizing::new(request.renewal_token);
            let Ok(tx) = database.client.transaction().await else {
                return unavailable();
            };
            let Ok(principal) =
                password_login::authenticate_renewal(&tx, &inner.issuer, &request.aud, &secret)
                    .await
            else {
                return unavailable();
            };
            let Some(principal) = principal else {
                // A replay refusal writes family revocation. Never roll it back.
                if tx.commit().await.is_err() {
                    return unavailable();
                }
                return if parts.uri.path() == "/password/logout" {
                    response(StatusCode::NO_CONTENT, "application/json", Vec::new())
                } else {
                    session::unauthorized()
                };
            };
            if parts.uri.path() != "/password/renew" {
                let result = if parts.uri.path() == "/password/logout-all" {
                    password_login::revoke_all(&tx, principal.principal().id()).await
                } else {
                    password_login::revoke_login(&tx, &inner.issuer, &request.aud, &secret).await
                };
                if result.is_err() || tx.commit().await.is_err() {
                    return unavailable();
                }
                return response(StatusCode::NO_CONTENT, "application/json", Vec::new());
            }
            let Some(target) = inner.targets.get(&request.aud) else {
                return session::unauthorized();
            };
            let claims = match session::claims_for_principal(inner, &principal, target).await {
                Ok(claims) => claims,
                Err(error) => return authority_failure(&error),
            };
            let renewal =
                match password_login::rotate_login(&tx, &inner.issuer, &request.aud, &secret).await
                {
                    Ok(Some(renewal)) => renewal,
                    Ok(None) => {
                        if tx.commit().await.is_err() {
                            return unavailable();
                        }
                        return session::unauthorized();
                    }
                    Err(_) => return unavailable(),
                };
            finish_session(tx, claims, renewal, started_at).await
        }
        _ => invalid(),
    }
}

fn authority_failure(error: &session::ExchangeFailure) -> Response<Full<Bytes>> {
    match error.kind {
        session::FailureKind::Unauthorized => session::unauthorized(),
        session::FailureKind::Unavailable => unavailable(),
    }
}

async fn finish_session(
    tx: tokio_postgres::Transaction<'_>,
    claims: wamn_platform_identity::session_token::SessionClaims,
    renewal: password_login::Renewal,
    started_at: i64,
) -> Response<Full<Bytes>> {
    let Ok(token) =
        sign_session_token_in_transaction(&tx, claims, started_at, Some(renewal.login.expires_at))
            .await
    else {
        return unavailable();
    };
    if tx.commit().await.is_err()
        || session::unix_seconds().is_none_or(|now| now >= token.claims().exp)
    {
        return unavailable();
    }
    renewable_response(&token, &renewal)
}

fn renewable_response(
    token: &IssuedSessionToken,
    renewal: &password_login::Renewal,
) -> Response<Full<Bytes>> {
    match serde_json::to_vec(&RenewableResponse {
        access_token: token.token(),
        token_type: "Bearer",
        expires_at: token.claims().exp,
        renewal_token: renewal.secret(),
        login_expires_at: renewal.login.expires_at,
    }) {
        Ok(bytes) => response(StatusCode::OK, "application/json", bytes),
        Err(_) => unavailable(),
    }
}

async fn admit(
    client: &mut Client,
    buckets: &[(&str, i32)],
    issuer: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let tx = client.transaction().await?;
    // Fixed-window resets do not extend on refused attempts. Global admission
    // bounds source/account row creation; pruning retains at most two minutes.
    let mut allowed = true;
    for (bucket, limit) in buckets {
        let count: i32 = tx.query_one("INSERT INTO identity.password_attempts (bucket, started_at, attempts) VALUES ($1, clock_timestamp(), 1) ON CONFLICT (bucket) DO UPDATE SET attempts = CASE WHEN identity.password_attempts.started_at <= EXCLUDED.started_at - interval '60 seconds' THEN 1 ELSE LEAST(identity.password_attempts.attempts + 1, $2 + 1) END, started_at = CASE WHEN identity.password_attempts.started_at <= EXCLUDED.started_at - interval '60 seconds' THEN EXCLUDED.started_at ELSE identity.password_attempts.started_at END RETURNING attempts", &[bucket, limit]).await?.get(0);
        if count > *limit {
            allowed = false;
            break;
        }
    }
    if buckets.first().is_some_and(|(key, _)| *key == "global") {
        let actor = PlatformComponent::Provisioning.principal_id().to_string();
        tx.execute("SELECT set_config('app.user_id',$1,true)", &[&actor])
            .await?;
        // Cleanup is service-owned and bounded independently of submitted credentials.
        password_login::prune_expired(&tx, issuer).await?;
        tx.execute("DELETE FROM identity.password_attempts WHERE started_at < clock_timestamp() - interval '120 seconds'", &[]).await?;
    }
    tx.commit().await?;
    Ok(allowed)
}
fn password_failure(error: &PasswordError) -> Response<Full<Bytes>> {
    match error.kind() {
        PasswordErrorKind::Busy => throttled(),
        PasswordErrorKind::Infrastructure => unavailable(),
        PasswordErrorKind::Policy | PasswordErrorKind::Refused => invalid(),
    }
}
fn invalid() -> Response<Full<Bytes>> {
    response(
        StatusCode::BAD_REQUEST,
        "application/json",
        br#"{"error":"password request refused"}"#.to_vec(),
    )
}
fn throttled() -> Response<Full<Bytes>> {
    let mut reply = response(
        StatusCode::TOO_MANY_REQUESTS,
        "application/json",
        br#"{"error":"try again later"}"#.to_vec(),
    );
    reply.headers_mut().insert(
        hyper::header::RETRY_AFTER,
        hyper::header::HeaderValue::from_static("60"),
    );
    reply
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IdentityConfig, IdentityService};
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use serde_json::json;
    use std::convert::Infallible;
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use wamn_control_provision::CredentialGeneration;
    use wamn_control_provision::identity_issuer::{
        identity_issuer_generation_role, prepare_identity_issuer_generation_sql,
    };

    #[tokio::test]
    async fn operator_invitation_delivery_failure_and_shared_admission() {
        let mut pg = wamn_test_postgres::start(&[]).expect("owned PostgreSQL");
        let db = pg.create_database("wamn_system").unwrap();
        let admin = connect_database(db.url()).await.unwrap();
        admin
            .client
            .batch_execute(wamn_control_provision::sql::ensure_db_owner_role_sql())
            .await
            .unwrap();
        admin.client.batch_execute("CREATE ROLE wamn_system NOLOGIN; GRANT CREATE ON DATABASE wamn_system TO wamn_system; SET ROLE wamn_system").await.unwrap();
        admin
            .client
            .batch_execute(wamn_control_provision::SYSTEM_SCHEMA_SQL)
            .await
            .unwrap();
        admin.client.batch_execute("RESET ROLE").await.unwrap();
        let issuer = "https://password-fixture.invalid";
        admin
            .client
            .batch_execute(
                &prepare_identity_issuer_generation_sql(
                    issuer,
                    CredentialGeneration::A,
                    "fixture-password",
                    "2100-01-01T00:00:00Z",
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let actor = PlatformComponent::Provisioning.principal_id().to_string();
        admin
            .client
            .execute("SELECT set_config('app.user_id', $1, false)", &[&actor])
            .await
            .unwrap();
        let alice = wamn_platform_identity::create_human(
            &admin.client,
            "alice",
            "alice@example.invalid",
            "Alice",
        )
        .await
        .unwrap();
        let bob = wamn_platform_identity::create_human(
            &admin.client,
            "bob",
            "bob@example.invalid",
            "Bob",
        )
        .await
        .unwrap();
        let mut url = url::Url::parse(db.url()).unwrap();
        url.set_username(
            &identity_issuer_generation_role(issuer, CredentialGeneration::A).unwrap(),
        )
        .unwrap();
        url.set_password(Some("fixture-password")).unwrap();
        let mut service = IdentityService::connect(
            IdentityConfig::new(issuer, url.as_str())
                .unwrap()
                .with_resend(
                    ResendConfig::new("unused".into(), "WAMN <fixture@example.invalid>".into())
                        .unwrap(),
                ),
        )
        .await
        .unwrap();
        let mail_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mail_endpoint = format!("http://{}/emails", mail_listener.local_addr().unwrap());
        Arc::get_mut(&mut service.inner)
            .unwrap()
            .passwords
            .as_mut()
            .unwrap()
            .mail = Mailer::fixture(mail_endpoint);
        let (sent, mut received) = tokio::sync::mpsc::channel(2);
        let mail_task = tokio::spawn(async move {
            for status in [
                StatusCode::OK,
                StatusCode::INTERNAL_SERVER_ERROR,
                StatusCode::OK,
                StatusCode::OK,
            ] {
                let (tcp, _) = mail_listener.accept().await.unwrap();
                let sent = sent.clone();
                let handler = service_fn(move |request: Request<Incoming>| {
                    let sent = sent.clone();
                    async move {
                        assert_eq!(request.headers()["authorization"], "Bearer fixture-key");
                        let bytes = request.into_body().collect().await.unwrap().to_bytes();
                        sent.send(serde_json::from_slice::<serde_json::Value>(&bytes).unwrap())
                            .await
                            .unwrap();
                        Ok::<_, Infallible>(response(status, "application/json", b"{}".to_vec()))
                    }
                });
                // Close after each response so the next request gets its own status.
                hyper::server::conn::http1::Builder::new()
                    .keep_alive(false)
                    .serve_connection(TokioIo::new(tcp), handler)
                    .await
                    .unwrap();
            }
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let requests = tokio::spawn(async move {
            for _ in 0..7 {
                let (tcp, _) = listener.accept().await.unwrap();
                let service = service.clone();
                let handler = service_fn(move |request| {
                    let service = service.clone();
                    async move {
                        service
                            .respond(request, true, "127.0.0.1".parse().unwrap())
                            .await
                    }
                });
                hyper::server::conn::http1::Builder::new()
                    .keep_alive(false)
                    .serve_connection(TokioIo::new(tcp), handler)
                    .await
                    .unwrap();
            }
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let reply = client
            .post(format!("{endpoint}/invitations"))
            .json(&json!({"principal_id":alice.id().as_str()}))
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status(), 201);
        assert_eq!(
            reply.json::<serde_json::Value>().await.unwrap()["status"],
            "accepted_for_delivery"
        );
        let mail = received.recv().await.unwrap();
        assert_eq!(mail["to"][0], "alice@example.invalid");
        let text = mail["text"].as_str().unwrap();
        let secret = text
            .lines()
            .find_map(|l| l.strip_prefix("Invitation secret: "))
            .unwrap();
        let reply = client.post(format!("{endpoint}/password/enroll")).json(&json!({"principal_id":alice.id().as_str(),"invitation":secret,"password":"a long enough password"})).send().await.unwrap();
        assert_eq!(reply.status(), 204);
        let reply = client
            .post(format!("{endpoint}/invitations"))
            .json(&json!({"principal_id":bob.id().as_str()}))
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status(), 503);
        assert_eq!(
            received.recv().await.unwrap()["to"][0],
            "bob@example.invalid"
        );
        let remaining: i64 = admin
            .client
            .query_one(
                "SELECT count(*) FROM identity.password_tokens WHERE consumed_at IS NULL",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(remaining, 0);
        let unknown = client
            .post(format!("{endpoint}/password/recover"))
            .json(&json!({"email":"unknown@example.invalid"}))
            .send()
            .await
            .unwrap();
        assert_eq!(unknown.status(), 202);
        let unknown_body = unknown.bytes().await.unwrap();
        let recovery = client
            .post(format!("{endpoint}/password/recover"))
            .json(&json!({"email":"alice@example.invalid"}))
            .send()
            .await
            .unwrap();
        assert_eq!(recovery.status(), 202);
        assert_eq!(recovery.bytes().await.unwrap(), unknown_body);
        let mail = received.recv().await.unwrap();
        assert_eq!(mail["subject"], "Reset your WAMN password");
        let secret = mail["text"]
            .as_str()
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("Reset secret: "))
            .unwrap();
        let reset_body = json!({"email":"alice@example.invalid", "secret":secret, "password":"a replacement long password"});
        let reset = client
            .post(format!("{endpoint}/password/reset"))
            .json(&reset_body)
            .send()
            .await
            .unwrap();
        assert_eq!(reset.status(), 200);
        assert_eq!(
            reset.json::<serde_json::Value>().await.unwrap()["notification"],
            "accepted_for_delivery"
        );
        let notification = received.recv().await.unwrap();
        assert_eq!(notification["subject"], "Your WAMN password changed");
        assert!(!notification["text"].as_str().unwrap().contains(secret));
        let repeated = client
            .post(format!("{endpoint}/password/reset"))
            .json(&reset_body)
            .send()
            .await
            .unwrap();
        assert_eq!(repeated.status(), 400);
        mail_task.await.unwrap();
        requests.await.unwrap();
        // Concurrent requests through separate scoped connections share the limit.
        let mut jobs = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let url = url.to_string();
            jobs.spawn(async move {
                let mut db = connect_database(&url).await.unwrap();
                admit(
                    &mut db.client,
                    &[("race-account", 5)],
                    "https://fixture.invalid",
                )
                .await
                .unwrap()
            });
        }
        let mut allowed = 0;
        while let Some(result) = jobs.join_next().await {
            allowed += usize::from(result.unwrap());
        }
        assert_eq!(allowed, 5);
    }
}
