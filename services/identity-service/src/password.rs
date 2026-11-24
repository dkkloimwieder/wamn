//! Password endpoints share bounded work, database throttling, and session authority.

use std::net::IpAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::{Request, Response, StatusCode, body::Incoming};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection as Client;
use wamn_authentication::password::{
    Password, PasswordError, PasswordErrorType, PasswordWork, RefusalCause, password_work,
};
use wamn_authentication::password_requests::{
    self, Attempt, DeliveryFailure, EmailDelivery, PasswordStore as _,
};
use wamn_authentication::session_exchange::{
    self, Exchange, ExchangeFailure, RenewalAction, Session,
};
use wamn_identity_protocol::PrincipalId;
use wamn_postgres::identity::issuer::Issuer;

use zeroize::Zeroizing;

use crate::{
    IdentityServiceError, Inner,
    mail::{MailConfig, Mailer},
    response, session, unavailable,
};

// The service admits at most two password hashing jobs per replica, with no queue.
// Database windows cap the whole issuer at 120/minute, source at 20, account at 5.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REQUEST_BYTES: usize = 8192;

#[derive(Debug)]
pub(super) struct State {
    pub(super) mail: Mailer,
    pool: sqlx::PgPool,
    work: PasswordWork,
}
impl State {
    pub(super) async fn new(
        mail: MailConfig,
        connection: &str,
    ) -> Result<Self, IdentityServiceError> {
        Ok(Self {
            mail: Mailer::new(mail)?,
            pool: wamn_postgres::pool(connection, 8).await.map_err(|_| {
                IdentityServiceError::new("identity password pool configuration refused")
            })?,
            work: password_work(),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollmentRequest {
    principal_id: wamn_identity_protocol::PrincipalId,
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
    sub: &'a PrincipalId,
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
    source: IpAddr,
) -> Response<Full<Bytes>> {
    let Ok(_permit) = inner.request_slots.try_acquire() else {
        return throttled();
    };
    let Some(started_at) = session_exchange::unix_seconds() else {
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
    // SQLx flushes pending transaction rollbacks before returning a session to the pool.
    let Ok(mut database) = state.pool.acquire().await else {
        return unavailable();
    };
    match password_requests::admit(
        &mut Issuer(&mut database),
        &inner.issuer,
        Attempt::Source(source),
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
        "/password/enroll" => {
            let Ok(request) = serde_json::from_slice::<EnrollmentRequest>(&bytes) else {
                return invalid();
            };
            let secret = Zeroizing::new(request.invitation);
            let principal = request.principal_id;
            let password = match Password::new(request.password) {
                Ok(value) => value,
                Err(error) => {
                    return enrollment_refusal(&mut database, &principal, &error).await;
                }
            };
            match password_requests::admit(
                &mut Issuer(&mut database),
                &inner.issuer,
                Attempt::Enrollment(&principal),
            )
            .await
            {
                Ok(true) => (),
                Ok(false) => return throttled(),
                Err(_) => return unavailable(),
            }
            match Issuer(&mut database)
                .enroll(&state.work, &principal, &secret, password)
                .await
            {
                Ok(()) => response(StatusCode::NO_CONTENT, "application/json", Vec::new()),
                Err(error) => enrollment_refusal(&mut database, &principal, &error).await,
            }
        }
        "/password/session" => {
            let Ok(LoginRequest {
                email,
                password,
                aud: audience,
            }) = serde_json::from_slice(&bytes)
            else {
                return invalid();
            };
            let Ok(password) = Password::new(password) else {
                return session::unauthorized();
            };
            let Some(email) = password_requests::password_email(&email) else {
                return session::unauthorized();
            };
            match password_requests::admit(
                &mut Issuer(&mut database),
                &inner.issuer,
                Attempt::Login(&email),
            )
            .await
            {
                Ok(true) => (),
                Ok(false) => return throttled(),
                Err(_) => return unavailable(),
            }
            let exchange = Exchange {
                issuer: &inner.issuer,
                audience: &audience,
                started_at,
                envelope: &inner.envelope,
            };
            match session_exchange::login(
                &mut Issuer(&mut database),
                exchange,
                &state.work,
                &email,
                password,
            )
            .await
            {
                Ok(session) => renewable_response(&session),
                Err(error) => exchange_failure(&error),
            }
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
            let Some(email) = password_requests::password_email(&email) else {
                return invalid();
            };
            let mut store = Issuer(&mut database);
            match password_requests::admit(&mut store, &inner.issuer, Attempt::Recovery(&email))
                .await
            {
                Ok(true) => (),
                Ok(false) => return throttled(),
                Err(_) => return unavailable(),
            }
            if let Some((secret, password)) = reset {
                match password_requests::reset(&mut store, &state.work, &email, &secret, password)
                    .await
                {
                    Ok(()) => (),
                    Err(error) => return delivery_failure(&error, &email),
                }
                let notified = state.mail.password_changed(&email).await.is_ok();
                return response(StatusCode::OK, "application/json", serde_json::to_vec(&serde_json::json!({"status":"password_reset", "notification": if notified { "accepted_for_delivery" } else { "unavailable" }})).expect("fixed response"));
            }
            password_requests::pad_recovery(async {
                match password_requests::recover(&mut store, &email).await {
                    Ok(token) => {
                        let delivery = async {
                            state
                                .mail
                                .reset(&email, token.secret(), token.expires_at())
                                .await
                                .is_ok()
                        };
                        let _ = password_requests::deliver(
                            &mut store,
                            &token,
                            EmailDelivery::Recovery,
                            delivery,
                        )
                        .await;
                    }
                    Err(error) => {
                        let _ = delivery_failure(&error, &email);
                    }
                }
            })
            .await;
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
            let exchange = Exchange {
                issuer: &inner.issuer,
                audience: &request.aud,
                started_at,
                envelope: &inner.envelope,
            };
            let action = match parts.uri.path() {
                "/password/renew" => RenewalAction::Renew,
                "/password/logout" => RenewalAction::Logout,
                _ => RenewalAction::LogoutAll,
            };
            match session_exchange::renewal(&mut Issuer(&mut database), exchange, action, &secret)
                .await
            {
                Ok(Some(session)) => renewable_response(&session),
                Ok(None) => logged_out(),
                Err(error) => exchange_failure(&error),
            }
        }
        _ => invalid(),
    }
}

fn exchange_failure(error: &ExchangeFailure) -> Response<Full<Bytes>> {
    match error {
        ExchangeFailure::Unauthorized => session::unauthorized(),
        ExchangeFailure::Unavailable => unavailable(),
        ExchangeFailure::Password(error) => password_failure(error),
    }
}

fn delivery_failure(error: &DeliveryFailure<'_>, email: &str) -> Response<Full<Bytes>> {
    match error {
        DeliveryFailure::Unavailable => unavailable(),
        DeliveryFailure::UnknownPrincipal => {
            tracing::info!(email = %email, cause = %RefusalCause::NoSuchAccount, "password request refused");
            invalid()
        }
        DeliveryFailure::Refused { error, email } => refusal(error, email),
    }
}

fn renewable_response(Session { token, renewal }: &Session) -> Response<Full<Bytes>> {
    match serde_json::to_vec(&RenewableResponse {
        sub: &token.claims().sub,
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

fn logged_out() -> Response<Full<Bytes>> {
    response(StatusCode::NO_CONTENT, "application/json", Vec::new())
}

/// Log the cause of a refusal with the account's email, then answer as
/// `password_failure`. The wire text stays the same for every cause.
pub(super) fn refusal(error: &PasswordError, email: &str) -> Response<Full<Bytes>> {
    log_refusal(error, email);
    password_failure(error)
}

fn log_refusal(error: &PasswordError, email: &str) {
    if let Some(cause) = error.cause() {
        if let Some(reason) = error.reason() {
            tracing::info!(email = %email, cause = %cause, reason, "password request refused");
        } else {
            tracing::info!(email = %email, cause = %cause, "password request refused");
        }
    }
}

/// An enrollment request names a principal, not an email, so look the email up
/// for the log line.
async fn enrollment_refusal(
    client: &mut Client,
    principal: &PrincipalId,
    error: &PasswordError,
) -> Response<Full<Bytes>> {
    if let Some(cause) = error.cause() {
        match Issuer(client).enrollment_email(principal).await {
            Ok(Some(email)) => return refusal(error, &email),
            Ok(None) => {
                tracing::info!(principal = %principal, cause = %cause, "password request refused");
            }
            Err(_) => (),
        }
    }
    password_failure(error)
}

fn password_failure(error: &PasswordError) -> Response<Full<Bytes>> {
    match error.error_type() {
        PasswordErrorType::Busy => throttled(),
        PasswordErrorType::Infrastructure => unavailable(),
        PasswordErrorType::Policy | PasswordErrorType::Refused => invalid(),
    }
}
fn invalid() -> Response<Full<Bytes>> {
    response(
        StatusCode::BAD_REQUEST,
        "application/json",
        br#"{"error":"password request refused"}"#.to_vec(),
    )
}
pub(super) fn throttled() -> Response<Full<Bytes>> {
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
pub(crate) mod tests {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    use super::*;
    use crate::connect_database;
    use crate::{IdentityConfig, IdentityService};
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use serde_json::json;
    use sqlx::{Connection as _, Row as _};
    use std::convert::Infallible;
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use wamn_http_client::HttpClient;
    use wamn_identity_protocol::PlatformComponent;
    use wamn_provision::identity_role::identity_issuer_login;
    use wamn_test_infrastructure::containers::{Postgres, start_postgres};

    /// Create the issuer login as CNPG would, with the identity issuer surface.
    fn issuer_login_sql(issuer: &str, password: &str) -> String {
        use postgres_protocol::escape::{escape_identifier, escape_literal};
        let role = identity_issuer_login(issuer).unwrap();
        let role = escape_identifier(role.as_str());
        format!(
            "{} CREATE ROLE {role} LOGIN INHERIT PASSWORD {}; GRANT wamn_identity_issuer TO {role} WITH ADMIN FALSE, INHERIT TRUE, SET FALSE; GRANT CONNECT ON DATABASE wamn_system TO {role};",
            wamn_provision::identity_role::grant_identity_issuer_surface_sql(),
            escape_literal(password),
        )
    }

    /// Install the control store with the issuer login and one active member
    /// of org `demo` per `(subject, email, display name)`. Return the
    /// administrator connection, the issuer login URL and the members.
    pub(crate) async fn demo_members(
        postgres: &Postgres,
        issuer: &str,
        users: &[(&str, &str, &str)],
    ) -> (
        crate::Database,
        url::Url,
        Vec<wamn_authentication::principal::Principal>,
    ) {
        let mut url = wamn_test_infrastructure::identity::system(postgres).await;
        let mut admin = connect_database(url.as_str()).await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(issuer_login_sql(
            issuer,
            "fixture-password",
        )))
        .execute(&mut admin.client)
        .await
        .map(drop)
        .unwrap();
        let actor = PlatformComponent::Provisioning.principal_id();
        sqlx::query("INSERT INTO platform.login_principal(login,principal_id) VALUES(current_user,$1) ON CONFLICT(login) DO UPDATE SET principal_id=EXCLUDED.principal_id")
            .bind(sqlx::types::Uuid::from(actor)).execute(&mut admin.client).await.unwrap();
        let mut members = Vec::new();
        for (subject, email, display_name) in users {
            members.push(
                wamn_postgres::identity::create_user(
                    &mut admin.client,
                    subject,
                    email,
                    display_name,
                )
                .await
                .unwrap(),
            );
        }
        let mut tx = admin.client.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO platform.tx_identity(principal_id,role,application) VALUES($1,'','')",
        )
        .bind(sqlx::types::Uuid::from(actor))
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query("INSERT INTO registry.orgs (id) VALUES ('demo')")
            .execute(&mut *tx)
            .await
            .unwrap();
        for member in &members {
            sqlx::query("INSERT INTO identity.org_memberships (principal_id, org, status) VALUES ($1, 'demo', 'active')")
                .bind(sqlx::types::Uuid::from(*member.id()))
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        url.set_username(identity_issuer_login(issuer).unwrap().as_str())
            .unwrap();
        url.set_password(Some("fixture-password")).unwrap();
        (admin, url, members)
    }

    #[tokio::test]
    #[ignore = "requires: docker"]
    async fn operator_invitation_recovery_and_shared_admission() {
        let postgres = start_postgres().await.unwrap();
        let issuer = "https://password-fixture.invalid";
        let (mut admin, url, members) = demo_members(
            &postgres,
            issuer,
            &[
                ("alice", "alice@example.invalid", "Alice"),
                ("bob", "bob@example.invalid", "Bob"),
            ],
        )
        .await;
        let [alice, bob] = <[_; 2]>::try_from(members).unwrap();
        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
        let key = scratch.path().join("envelope-key");
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&key)
            .unwrap()
            .write_all(&[42; 32])
            .unwrap();
        let mail_path = scratch.path().join("mail.jsonl");
        let mut service = IdentityService::connect(
            IdentityConfig::new(issuer, url.as_str())
                .unwrap()
                .with_local_mail(mail_path.clone()),
            wamn_key::from_file(&key).unwrap(),
        )
        .await
        .unwrap();
        let review_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let review_endpoint = format!("http://{}", review_listener.local_addr().unwrap());
        let state = Arc::get_mut(&mut service.inner).unwrap();
        let mut config = kube::Config::new(review_endpoint.parse().unwrap());
        config.default_namespace = "platform".into();
        config.auth_info.token = Some("fixture-reviewer".into());
        state.kubernetes =
            Some(wamn_kubernetes::client::KubernetesApi::from_config(config).unwrap());
        let review_task = tokio::spawn(async move {
            while let Ok((tcp, _)) = review_listener.accept().await {
                let handler = service_fn(|_: Request<Incoming>| async {
                    let status = json!({"status": {
                        "authenticated": true,
                        "audiences": [wamn_identity_client::TOKEN_AUDIENCE],
                        "user": {"username": "system:serviceaccount:demo:wamn-control-demo"},
                    }});
                    Ok::<_, Infallible>(response(
                        StatusCode::CREATED,
                        "application/json",
                        status.to_string().into_bytes(),
                    ))
                });
                hyper::server::conn::http1::Builder::new()
                    .keep_alive(false)
                    .serve_connection(TokioIo::new(tcp), handler)
                    .await
                    .unwrap();
            }
        });
        // Each delivered mail is the next line of the local mail file.
        let mut delivered = 0;
        let mut next_mail = || {
            let line = std::fs::read_to_string(&mail_path)
                .unwrap()
                .lines()
                .nth(delivered)
                .map(str::to_owned)
                .unwrap();
            delivered += 1;
            serde_json::from_str::<serde_json::Value>(&line).unwrap()
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let requests = tokio::spawn(async move {
            for _ in 0..12 {
                let (tcp, _) = listener.accept().await.unwrap();
                let service = service.clone();
                let handler = service_fn(move |request| {
                    let service = service.clone();
                    async move { service.respond(request, "127.0.0.1".parse().unwrap()).await }
                });
                hyper::server::conn::http1::Builder::new()
                    .keep_alive(false)
                    .serve_connection(TokioIo::new(tcp), handler)
                    .await
                    .unwrap();
            }
        });
        let client = HttpClient::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let reply = client
            .post(format!("{endpoint}/invitations"))
            .bearer_auth("fixture-token")
            .json(&json!({"principal_id":alice.id().to_string().as_str()}))
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status(), 201);
        assert_eq!(
            reply.json::<serde_json::Value>().await.unwrap()["status"],
            "accepted_for_delivery"
        );
        let mail = next_mail();
        assert_eq!(mail["to"], "alice@example.invalid");
        let text = mail["text"].as_str().unwrap();
        let secret = text
            .lines()
            .find_map(|l| l.strip_prefix("Invitation code: "))
            .and_then(|code| code.split_once(':').map(|(_, secret)| secret))
            .unwrap();
        let reply = client.post(format!("{endpoint}/password/enroll")).json(&json!({"principal_id":alice.id().to_string().as_str(),"invitation":secret,"password":"a long enough password"})).send().await.unwrap();
        assert_eq!(reply.status(), 204);
        // `/users` creates a user once, folds its email into the subject and
        // reports whether it has a password credential.
        let mut users = Vec::new();
        for (email, display_name) in [
            (" Carol@Example.Invalid ", "Carol"),
            ("carol@example.invalid", "Other"),
            ("alice@example.invalid", "Alice"),
            ("bob@example.invalid", "Bob"),
            ("not-an-email", "Nobody"),
        ] {
            let reply = client
                .post(format!("{endpoint}/users"))
                .bearer_auth("fixture-token")
                .json(&json!({"email": email, "display_name": display_name}))
                .send()
                .await
                .unwrap();
            users.push((
                reply.status().as_u16(),
                reply.json::<serde_json::Value>().await.unwrap(),
            ));
        }
        let carol = users[0].1["principal_id"].as_str().unwrap().to_owned();
        let alice_id = alice.id().to_string();
        let bob_id = bob.id().to_string();
        for ((status, body), (principal, enrolled)) in users[..4].iter().zip([
            (carol.as_str(), false),
            (carol.as_str(), false),
            (alice_id.as_str(), true),
            (bob_id.as_str(), false),
        ]) {
            assert_eq!(*status, 200);
            assert_eq!(body["principal_id"], principal);
            assert_eq!(body["enrolled"], enrolled);
        }
        assert_eq!(users[4].0, 400);
        assert!(users[4].1["error"].is_string());
        let row = sqlx::query(
            "SELECT subject, email, display_name, updated_by::text \
                 FROM identity.principals WHERE id = $1",
        )
        .bind(sqlx::types::Uuid::from(
            carol.parse::<PrincipalId>().unwrap(),
        ))
        .fetch_one(&mut admin.client)
        .await
        .unwrap();
        assert_eq!(
            (0..4).map(|index| row.get(index)).collect::<Vec<String>>(),
            [
                "carol@example.invalid",
                "carol@example.invalid",
                "Carol",
                &PlatformComponent::IdentityService
                    .principal_id()
                    .to_string()
            ]
        );
        wamn_postgres::identity::disable_principal(&mut admin.client, bob.id())
            .await
            .unwrap();
        let disabled = client
            .post(format!("{endpoint}/users"))
            .bearer_auth("fixture-token")
            .json(&json!({"email": "bob@example.invalid", "display_name": "Bob"}))
            .send()
            .await
            .unwrap();
        assert_eq!(disabled.status(), 409);
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
        let mail = next_mail();
        assert!(!mail["subject"].as_str().unwrap().is_empty());
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
        let notification = next_mail();
        assert!(!notification["subject"].as_str().unwrap().is_empty());
        assert!(!notification["text"].as_str().unwrap().contains(secret));
        let repeated = client
            .post(format!("{endpoint}/password/reset"))
            .json(&reset_body)
            .send()
            .await
            .unwrap();
        assert_eq!(repeated.status(), 400);
        review_task.abort();
        requests.await.unwrap();
    }
}
