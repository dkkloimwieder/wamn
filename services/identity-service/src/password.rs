//! Password endpoints share bounded work, database throttling, and session authority.

use sqlx::{Connection as _, Row as _};
use wamn_identity_protocol::PrincipalId;

use std::net::IpAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::{Request, Response, StatusCode, body::Incoming};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection as Client;
use wamn_identity::{
    password::{
        Password, PasswordError, PasswordErrorType, PasswordWork, RefusalCause,
        authenticate_password, enroll_password, issue_reset, password_work,
        reset_password,
    },
    password_login,
    session_token::{IssuedSessionToken, sign_session_token_in_transaction},
};
use wamn_provision::PlatformComponent;

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
    // SQLx flushes pending transaction rollbacks before returning a session to the pool.
    let Ok(mut database) = state.pool.acquire().await else {
        return unavailable();
    };
    match admit_source(&mut database, &inner.issuer, source).await {
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
                    return enrollment_refusal(&mut *database, &principal, &error).await;
                }
            };
            let bucket = format!("enroll:{principal}");
            match admit(&mut *database, &[(&bucket, 5)], &inner.issuer).await {
                Ok(true) => (),
                Ok(false) => return throttled(),
                Err(_) => return unavailable(),
            }
            match enroll_password(&mut *database, &state.work, &principal, &secret, password).await
            {
                Ok(()) => response(StatusCode::NO_CONTENT, "application/json", Vec::new()),
                Err(error) => enrollment_refusal(&mut *database, &principal, &error).await,
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
            if email.len() > 320 {
                return session::unauthorized();
            }
            let email = email.trim().to_lowercase();
            let bucket = format!(
                "login:{}",
                wamn_digest::bytes_sha256(email.as_bytes()).hex()
            );
            match admit(&mut *database, &[(&bucket, 5)], &inner.issuer).await {
                Ok(true) => (),
                Ok(false) => return throttled(),
                Err(_) => return unavailable(),
            }
            let Ok(mut read) = wamn_identity::transaction::read_connection(
                &mut *database,
                &PlatformComponent::IdentityService.principal_id(),
            )
            .await
            else {
                return unavailable();
            };
            let Ok(row) = sqlx::query(
                "SELECT id::text FROM identity.principals WHERE email=$1 AND type='user'",
            )
            .bind(&email)
            .fetch_optional(&mut *read)
            .await
            else {
                return unavailable();
            };
            let Some(row) = row else {
                let refused = authenticate_password(&mut read, &state.work, &email, password).await;
                if read.commit().await.is_err() {
                    return unavailable();
                }
                return match refused {
                    Ok(_) => session::unauthorized(),
                    Err(error) => password_failure(&error),
                };
            };
            if read.commit().await.is_err() {
                return unavailable();
            }
            let Ok(id) = row.get::<String, _>(0).parse::<PrincipalId>() else {
                return unavailable();
            };
            let Ok(mut tx) = database.begin().await else {
                return unavailable();
            };
            if wamn_postgres::bind_transaction_identity(
                &mut tx,
                &id,
                "",
                "identity",
                Some(&PlatformComponent::IdentityService.principal_name()),
                None,
            )
            .await
            .is_err()
            {
                return unavailable();
            }
            if password_login::lock_principal(&mut tx, &id).await.is_err() {
                return unavailable();
            }
            let principal =
                match authenticate_password(&mut tx, &state.work, &email, password).await {
                    Ok(Some(value)) => value,
                    Ok(None) => return session::unauthorized(),
                    Err(error) => return password_failure(&error),
                };
            let renewal = match password_login::create_login(
                &mut tx,
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
            let claims =
                match session::claims_for_audience(inner, &mut tx, &principal, &audience).await {
                    Ok(claims) => claims,
                    Err(error) => return authority_failure(&error),
                };
            finish_session(tx, claims, renewal, started_at, &inner.envelope).await
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
            let bucket = format!(
                "recovery:{}",
                wamn_digest::bytes_sha256(email.as_bytes()).hex()
            );
            match admit(&mut *database, &[(&bucket, 5)], &inner.issuer).await {
                Ok(true) => (),
                Ok(false) => return throttled(),
                Err(_) => return unavailable(),
            }
            // Pad the whole account-dependent recovery path, including provider failures.
            // No mail task outlives the request or performs automatic retries.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
            let action = async {
                let Ok(mut read) = wamn_identity::transaction::read_connection(
                    &mut *database,
                    &PlatformComponent::IdentityService.principal_id(),
                )
                .await
                else {
                    return unavailable();
                };
                let Ok(row) = sqlx::query("SELECT p.id FROM identity.principals p JOIN identity.password_credentials c ON c.principal_id=p.id WHERE p.email=$1 AND p.type='user' AND p.status='active'").bind(&email).fetch_optional(&mut *read).await else { return unavailable(); };
                if read.commit().await.is_err() {
                    return unavailable();
                }
                let Some(row) = row else {
                    tracing::info!(email = %email, cause = %RefusalCause::NoSuchAccount, "password request refused");
                    return invalid();
                };
                let principal: PrincipalId = row.get(0);
                if let Some((secret, password)) = reset {
                    let password = match Password::new(password) {
                        Ok(value) => value,
                        Err(error) => return refusal(&error, &email),
                    };
                    if let Err(error) =
                        reset_password(&mut *database, &state.work, &principal, &secret, password)
                            .await
                    {
                        return refusal(&error, &email);
                    }
                    let notified = state.mail.password_changed(&email).await.is_ok();
                    return response(StatusCode::OK, "application/json", serde_json::to_vec(&serde_json::json!({"status":"password_reset", "notification": if notified { "accepted_for_delivery" } else { "unavailable" }})).expect("fixed response"));
                }
                let actor: PrincipalId = PlatformComponent::IdentityService.principal_id();
                let token = match issue_reset(&mut *database, &actor, &principal, &email).await {
                    Ok(token) => token,
                    Err(error) => {
                        log_refusal(&error, &email);
                        return unavailable();
                    }
                };
                if state
                    .mail
                    .reset(&email, token.secret(), token.expires_at())
                    .await
                    .is_err()
                {
                    let hash = wamn_digest::bytes_sha256(token.secret().as_bytes());
                    if let Ok(mut tx) = database.begin().await
                        && wamn_postgres::bind_transaction_identity(
                            &mut tx,
                            &actor,
                            "",
                            "identity",
                            Some(&PlatformComponent::IdentityService.principal_name()),
                            None,
                        )
                        .await
                        .is_ok()
                        && sqlx::query("UPDATE identity.password_tokens SET consumed_at=clock_timestamp() WHERE token_hash=$1 AND consumed_at IS NULL").bind(hash).execute(&mut *tx).await.map(|result| result.rows_affected()).is_ok() { let _ = tx.commit().await; }
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
            let actor = match password_login::renewal_principal(
                &mut *database,
                &inner.issuer,
                &request.aud,
                &secret,
            )
            .await
            {
                Ok(Some(principal)) => principal,
                Ok(None) => {
                    return if parts.uri.path() == "/password/logout" {
                        logged_out()
                    } else {
                        session::unauthorized()
                    };
                }
                Err(_) => return unavailable(),
            };
            let Ok(mut tx) = database.begin().await else {
                return unavailable();
            };
            let Ok(principal) = password_login::authenticate_renewal(
                &mut tx,
                &actor,
                &inner.issuer,
                &request.aud,
                &secret,
            )
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
                    logged_out()
                } else {
                    session::unauthorized()
                };
            };
            if parts.uri.path() != "/password/renew" {
                let result = if parts.uri.path() == "/password/logout-all" {
                    password_login::revoke_all(&mut tx, principal.principal().id()).await
                } else {
                    password_login::revoke_login(&mut tx, &inner.issuer, &request.aud, &secret)
                        .await
                };
                if result.is_err() || tx.commit().await.is_err() {
                    return unavailable();
                }
                return logged_out();
            }
            let renewal =
                match password_login::rotate_login(&mut tx, &inner.issuer, &request.aud, &secret)
                    .await
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
            let claims = match session::claims_for_audience(
                inner,
                &mut tx,
                &principal,
                &request.aud,
            )
            .await
            {
                Ok(claims) => claims,
                Err(error) => return authority_failure(&error),
            };
            finish_session(tx, claims, renewal, started_at, &inner.envelope).await
        }
        _ => invalid(),
    }
}

fn authority_failure(error: &session::ExchangeFailure) -> Response<Full<Bytes>> {
    match error.type_ {
        session::FailureType::Unauthorized => session::unauthorized(),
        session::FailureType::Unavailable => unavailable(),
    }
}

async fn finish_session(
    mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
    claims: wamn_identity::session_token::AccessTokenClaims,
    renewal: password_login::Renewal,
    started_at: i64,
    envelope: &wamn_envelope::encryption::Envelope,
) -> Response<Full<Bytes>> {
    let Ok(token) = sign_session_token_in_transaction(
        &mut tx,
        claims,
        started_at,
        Some(renewal.login.expires_at),
        envelope,
    )
    .await
    else {
        return unavailable();
    };
    if tx.commit().await.is_err() {
        return unavailable();
    }
    if !session::unix_seconds().is_some_and(|now| now < token.claims().exp) {
        return unavailable();
    }
    renewable_response(&token, &renewal)
}

fn renewable_response(
    token: &IssuedSessionToken,
    renewal: &password_login::Renewal,
) -> Response<Full<Bytes>> {
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

pub(super) async fn admit_source(
    client: &mut Client,
    issuer: &str,
    source: IpAddr,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let source = match source {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or(source, IpAddr::V4),
        IpAddr::V4(_) => source,
    };
    let source_key = format!("source:{source}");
    admit(client, &[("global", 120), (&source_key, 20)], issuer).await
}

async fn admit(
    client: &mut Client,
    buckets: &[(&str, i32)],
    issuer: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let mut tx = client.begin().await?;
    let actor = PlatformComponent::IdentityService;
    wamn_postgres::bind_transaction_identity(
        &mut tx,
        &actor.principal_id(),
        "",
        "identity",
        Some(&actor.principal_name()),
        None,
    )
    .await?;

    // Fixed-window resets do not extend on refused attempts. Global admission
    // bounds source/account row creation; pruning retains at most two minutes.
    let mut allowed = true;
    for (bucket, limit) in buckets {
        let count: i32 = sqlx::query("INSERT INTO identity.password_attempts (bucket, started_at, attempts) VALUES ($1, clock_timestamp(), 1) ON CONFLICT (bucket) DO UPDATE SET attempts = CASE WHEN identity.password_attempts.started_at <= EXCLUDED.started_at - interval '60 seconds' THEN 1 ELSE LEAST(identity.password_attempts.attempts + 1, $2 + 1) END, started_at = CASE WHEN identity.password_attempts.started_at <= EXCLUDED.started_at - interval '60 seconds' THEN EXCLUDED.started_at ELSE identity.password_attempts.started_at END RETURNING attempts").bind(bucket).bind(limit).fetch_one(&mut *tx).await?.get(0);
        if count > *limit {
            allowed = false;
            break;
        }
    }
    if buckets.first().is_some_and(|(key, _)| *key == "global") {
        // Cleanup is service-owned and bounded independently of submitted credentials.
        password_login::prune_expired(&mut tx, issuer).await?;
        sqlx::query("DELETE FROM identity.password_attempts WHERE started_at < clock_timestamp() - interval '120 seconds'").execute(&mut *tx).await.map(|result| result.rows_affected())?;
    }
    tx.commit().await?;
    Ok(allowed)
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
        let Ok(mut read) =
            wamn_identity::transaction::read_connection(client, principal).await
        else {
            return password_failure(error);
        };
        match sqlx::query("SELECT email FROM identity.principals WHERE id = $1")
            .bind(principal)
            .fetch_optional(&mut *read)
            .await
        {
            Ok(Some(row)) => {
                let _ = read.commit().await;
                return refusal(error, row.get::<&str, _>(0));
            }
            _ => {
                tracing::info!(principal = %principal, cause = %cause, "password request refused");
            }
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
mod tests {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    use super::*;
    use crate::connect_database;
    use crate::{IdentityConfig, IdentityService};
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use serde_json::json;
    use std::convert::Infallible;
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use wamn_http_client::HttpClient;
    use wamn_provision::identity_role::identity_issuer_login;

    #[sqlx::test(migrations = false)]
    async fn admission_stamps_every_bucket_as_the_platform_principal(pool: sqlx::PgPool) {
        let _lock = wamn_test_postgres::lock();
        let database = wamn_test_postgres::url(&pool);
        let mut client = wamn_postgres::connect(database.as_str()).await.unwrap();

        sqlx::raw_sql("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN CREATE ROLE wamn_system; END IF; END $$").execute(&mut client).await.map(drop).unwrap();
        wamn_provision::test_database::system_schema(database.as_str()).await;
        for buckets in [
            vec![("global", 10)],
            vec![("source:test", 5), ("account:test", 5)],
        ] {
            assert!(
                super::admit(&mut client, &buckets, "https://history.invalid")
                    .await
                    .unwrap()
            );
            assert!(
                super::admit(&mut client, &buckets, "https://history.invalid")
                    .await
                    .unwrap()
            );
        }
        let actor = PlatformComponent::IdentityService.principal_id();
        wamn_test_infrastructure::record_history::assert_stamp(
            &mut client,
            "identity.password_attempts",
            "true",
            &actor,
            &actor,
        )
        .await;
        assert_eq!(
            sqlx::query("SELECT count(*) FROM identity.password_attempts")
                .fetch_one(&mut client)
                .await
                .unwrap()
                .get::<i64, _>(0),
            3
        );
    }

    #[tokio::test]
    #[ignore = "requires: postgres"]
    async fn operator_invitation_delivery_failure_and_shared_admission() {
        let mut pg = wamn_test_postgres::start(&[]).expect("owned PostgreSQL");
        let db = pg.create_database("wamn_system").await.unwrap();
        let mut admin = connect_database(db.url()).await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (wamn_provision::sql::ensure_project_roles_sql()).to_string(),
        ))
        .execute(&mut admin.client)
        .await
        .map(drop)
        .unwrap();
        sqlx::raw_sql("CREATE ROLE wamn_system NOLOGIN; GRANT CREATE ON DATABASE wamn_system TO wamn_system; SET ROLE wamn_system").execute(&mut admin.client).await.map(drop).unwrap();
        sqlx::raw_sql(wamn_provision::SYSTEM_SCHEMA_SQL)
            .execute(&mut admin.client)
            .await
            .map(drop)
            .unwrap();
        sqlx::raw_sql("RESET ROLE")
            .execute(&mut admin.client)
            .await
            .map(drop)
            .unwrap();
        let issuer = "https://password-fixture.invalid";
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            wamn_provision::test_database::issuer_login_sql(issuer, "fixture-password").unwrap(),
        ))
        .execute(&mut admin.client)
        .await
        .map(drop)
        .unwrap();
        let actor = PlatformComponent::Provisioning.principal_id();
        sqlx::query("INSERT INTO platform.login_principal(login,principal_id) VALUES(current_user,$1) ON CONFLICT(login) DO UPDATE SET principal_id=EXCLUDED.principal_id")
            .bind(actor).execute(&mut admin.client).await.unwrap();

        let alice = wamn_identity::create_user(
            &mut admin.client,
            "alice",
            "alice@example.invalid",
            "Alice",
        )
        .await
        .unwrap();
        let bob =
            wamn_identity::create_user(&mut admin.client, "bob", "bob@example.invalid", "Bob")
                .await
                .unwrap();
        let mut tx = admin.client.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO platform.tx_identity(principal_id,role,application) VALUES($1,'','')",
        )
        .bind(actor)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query("INSERT INTO registry.orgs (id) VALUES ('demo')")
            .execute(&mut *tx)
            .await
            .unwrap();
        for user in [alice.id(), bob.id()] {
            sqlx::query("INSERT INTO identity.org_memberships (principal_id, org, status) VALUES ($1, 'demo', 'active')")
                .bind(user)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();

        let mut url = url::Url::parse(db.url()).unwrap();
        url.set_username(identity_issuer_login(issuer).unwrap().as_str())
            .unwrap();
        url.set_password(Some("fixture-password")).unwrap();
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
        let mut service = IdentityService::connect(
            IdentityConfig::new(issuer, url.as_str())
                .unwrap()
                .with_resend(
                    crate::mail::ResendConfig::new(
                        "unused".into(),
                        "WAMN <fixture@example.invalid>".into(),
                    )
                    .unwrap(),
                ),
            wamn_envelope::encryption::Envelope::from_file(&key).unwrap(),
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
        let mail_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mail_endpoint = format!("http://{}/emails", mail_listener.local_addr().unwrap());
        state.passwords.as_mut().unwrap().mail = Mailer::fixture(mail_endpoint);
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
            for _ in 0..13 {
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
        let mail = received.recv().await.unwrap();
        assert_eq!(mail["to"][0], "alice@example.invalid");
        let text = mail["text"].as_str().unwrap();
        let secret = text
            .lines()
            .find_map(|l| l.strip_prefix("Invitation code: "))
            .and_then(|code| code.split_once(':').map(|(_, secret)| secret))
            .unwrap();
        let reply = client.post(format!("{endpoint}/password/enroll")).json(&json!({"principal_id":alice.id().to_string().as_str(),"invitation":secret,"password":"a long enough password"})).send().await.unwrap();
        assert_eq!(reply.status(), 204);
        let reply = client
            .post(format!("{endpoint}/invitations"))
            .bearer_auth("fixture-token")
            .json(&json!({"principal_id":bob.id().to_string().as_str()}))
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status(), 503);
        assert_eq!(
            received.recv().await.unwrap()["to"][0],
            "bob@example.invalid"
        );
        let remaining: i64 =
            sqlx::query("SELECT count(*) FROM identity.password_tokens WHERE consumed_at IS NULL")
                .fetch_one(&mut admin.client)
                .await
                .unwrap()
                .get(0);
        assert_eq!(remaining, 0);
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
        .bind(carol.parse::<PrincipalId>().unwrap())
        .fetch_one(&mut admin.client)
        .await
        .unwrap();
        assert_eq!(
            (0..4).map(|index| row.get(index)).collect::<Vec<String>>(),
            [
                "carol@example.invalid",
                "carol@example.invalid",
                "Carol",
                &actor.to_string()
            ]
        );
        wamn_identity::disable_principal(&mut admin.client, bob.id())
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
        let mail = received.recv().await.unwrap();
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
        let notification = received.recv().await.unwrap();
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
        mail_task.await.unwrap();
        requests.await.unwrap();
        // Concurrent requests through separate authorized connections share the limit.
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
