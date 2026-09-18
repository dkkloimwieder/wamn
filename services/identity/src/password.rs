//! Password endpoints share bounded work, database throttling, and session authority.

use std::net::IpAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::{Request, Response, StatusCode, body::Incoming};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tokio::sync::Semaphore;
use tokio_postgres::Client;
use wamn_control_provision::{PlatformComponent, identity_issuer::IdentityIssuerConnection};
use wamn_platform_identity::{
    PrincipalId,
    password::{
        Password, PasswordError, PasswordErrorKind, PasswordWork, authenticate_password,
        enroll_password, issue_invitation, password_work,
    },
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
struct LoginRequest {
    email: String,
    password: String,
    aud: String,
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
    match admit(&mut database.client, &[("global", 120), (&source_key, 20)]).await {
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
            match admit(&mut database.client, &[(&bucket, 5)]).await {
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
        "/password/session" => {
            let Ok(request) = serde_json::from_slice::<LoginRequest>(&bytes) else {
                return invalid();
            };
            let Ok(password) = Password::new(request.password) else {
                return session::unauthorized();
            };
            if request.email.len() > 320 {
                return session::unauthorized();
            }
            let email = request.email.trim().to_lowercase();
            let bucket = format!("login:{}", hex::encode(Sha256::digest(email.as_bytes())));
            match admit(&mut database.client, &[(&bucket, 5)]).await {
                Ok(true) => (),
                Ok(false) => return throttled(),
                Err(_) => return unavailable(),
            }
            let principal = match authenticate_password(
                &database.client,
                &state.work,
                &email,
                password,
            )
            .await
            {
                Ok(Some(value)) => value,
                Ok(None) => return session::unauthorized(),
                Err(error) => return password_failure(&error),
            };
            let Some(target) = inner.targets.get(&request.aud) else {
                return session::unauthorized();
            };
            match session::mint_for_principal(inner, &principal, target, started_at).await {
                Ok(token) => session::token_response(&token),
                Err(error) => match error.kind {
                    session::FailureKind::Unauthorized => session::unauthorized(),
                    session::FailureKind::Unavailable => unavailable(),
                },
            }
        }
        _ => invalid(),
    }
}

async fn admit(
    client: &mut Client,
    buckets: &[(&str, i32)],
) -> Result<bool, tokio_postgres::Error> {
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
            for status in [StatusCode::OK, StatusCode::INTERNAL_SERVER_ERROR] {
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
            for _ in 0..3 {
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
        mail_task.await.unwrap();
        requests.await.unwrap();
        // Concurrent requests through separate scoped connections share the limit.
        let mut jobs = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let url = url.to_string();
            jobs.spawn(async move {
                let mut db = connect_database(&url).await.unwrap();
                admit(&mut db.client, &[("race-account", 5)]).await.unwrap()
            });
        }
        let mut allowed = 0;
        while let Some(result) = jobs.join_next().await {
            allowed += usize::from(result.unwrap());
        }
        assert_eq!(allowed, 5);
    }
}
