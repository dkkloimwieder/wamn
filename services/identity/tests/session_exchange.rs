//! Real HTTPS exchanges against explicitly armed, disposable PostgreSQL 18.
//!
//! This suite replaces the system schemas and creates three exact fixture
//! databases. It uses the production issuer and role-reader credential builders.
//! Tokens and database credentials must never appear in assertion diagnostics.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};
use wamn_control_provision::identity_issuer::{
    IDENTITY_ISSUER_ROLE, grant_identity_issuer_surface_sql, identity_issuer_generation_role,
    prepare_identity_issuer_generation_sql,
};
use wamn_control_provision::project_env_database_name;
use wamn_control_provision::session_target::SessionTarget;
use wamn_control_provision::sql::{
    grant_session_role_reader_surface_sql, prepare_workload_generation_sql,
    revoke_public_connect_floor_sql,
};
use wamn_control_provision::workload_role::{WorkloadRoleScope, workload_generation_role};
use wamn_control_provision::{CredentialGeneration, WorkloadRoleFamily};
use wamn_control_registry::Triple;
use wamn_identity::{IdentityConfig, IdentityService, serve, tls_config};
use wamn_pg_core::quote_ident;
use wamn_platform_identity::session_keys::{
    SessionJwks, activate_session_key, publish_session_key,
};
use wamn_platform_identity::session_token::{
    MAXIMUM_LIFETIME, SessionClaims, SessionScope, session_key_id, verify_session_token,
};
use wamn_platform_identity::{
    Principal, create_human, create_service, disable_principal, grant_project_env_membership,
    issue_pat, revoke_pat, revoke_project_env_membership,
};

const ISSUER: &str = "https://identity.session-test.internal";
const PASSWORD: &str = "session-exchange-disposable-fixture-password";
const TENANT: &str = "session-tenant-a";
const OTHER_TENANT: &str = "session-tenant-b";
const SUFFIX: &str = "s3ss10n2";
const SCOPES: [(&str, &str); 3] = [("acme", "dev"), ("acme", "prod"), ("other", "dev")];

// A failing fixture or mutated response must not append upstream error Debug:
// PostgreSQL DETAIL and deserialization errors can contain secret input values.
trait ExpectRedacted<T> {
    fn expect_redacted(self, context: &'static str) -> T;
}

impl<T, E> ExpectRedacted<T> for Result<T, E> {
    fn expect_redacted(self, context: &'static str) -> T {
        self.ok().expect(context)
    }
}

impl<T> ExpectRedacted<T> for Option<T> {
    fn expect_redacted(self, context: &'static str) -> T {
        self.expect(context)
    }
}

struct Database {
    client: Client,
    driver: JoinHandle<()>,
}

impl Drop for Database {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

struct Fixture {
    system: Database,
    environments: Vec<Database>,
    targets: Vec<SessionTarget>,
    issuer_url: String,
    issuer_role: String,
    roles: Vec<String>,
}

struct Https {
    endpoint: String,
    client: reqwest::Client,
    serving: JoinHandle<Result<(), wamn_identity::IdentityServiceError>>,
}

impl Drop for Https {
    fn drop(&mut self) {
        self.serving.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires armed disposable PostgreSQL 18; replaces system schemas and three named databases"]
async fn session_exchange_uses_fresh_scoped_authority_without_session_state() {
    let fixture = setup().await;
    let duplicate = IdentityConfig::new(ISSUER, &fixture.issuer_url)
        .expect_redacted("validated duplicate-target control")
        .with_session_targets(vec![fixture.targets[0].clone(), fixture.targets[0].clone()]);
    assert!(
        duplicate.is_err(),
        "duplicate audiences are refused before service connection even with identical credentials"
    );
    let disabled = start(&fixture, false).await;
    let response = disabled
        .client
        .post(format!("{}/session", disabled.endpoint))
        .header("content-type", "application/json")
        .body("{\"aud\":\"absent\"}")
        .send()
        .await
        .expect_redacted("foundation-only route control");
    assert_eq!(response.status(), 404);
    assert!(
        response.headers()["cache-control"] == "no-store",
        "response must not be cached"
    );
    drop(disabled);

    let https = start(&fixture, true).await;
    exercise(&fixture, &https).await;
    https.serving.abort();
    // Dropping the listener future aborts all its owned HTTPS connections.
    drop(https);
    cleanup(fixture).await;
}

async fn exercise(fixture: &Fixture, https: &Https) {
    let system = &fixture.system.client;
    let dev = &fixture.environments[0].client;
    let alice = create_human(system, "session-alice", "Session Alice")
        .await
        .expect_redacted("human fixture");
    let bob = create_human(system, "session-bob", "Session Bob")
        .await
        .expect_redacted("other human fixture");
    let service = create_service(system, "session-machine", "Session Machine")
        .await
        .expect_redacted("service fixture");
    let alice_pat = issue_pat(
        system,
        alice.id(),
        "session test",
        Duration::from_secs(3600),
    )
    .await
    .expect_redacted("human PAT");
    let bob_pat = issue_pat(system, bob.id(), "session test", Duration::from_secs(3600))
        .await
        .expect_redacted("other PAT");
    let service_pat = issue_pat(
        system,
        service.id(),
        "session test",
        Duration::from_secs(3600),
    )
    .await
    .expect_redacted("service PAT");
    for (index, role) in ["receiver", "prod-reader", "other-reader"]
        .into_iter()
        .enumerate()
    {
        seed_user(&fixture.environments[index].client, &alice, TENANT, role).await;
    }
    seed_user(dev, &alice, OTHER_TENANT, "tenant-outsider").await;
    seed_user(dev, &bob, TENANT, "auditor").await;
    seed_user(dev, &service, TENANT, "machine-role").await;
    grant(system, &bob, "acme", "dev").await;
    let aud = fixture.targets[0].audience();

    // Tenant data and a management role are insufficient without an explicit
    // membership. The second principal's membership cannot admit Alice either.
    wamn_platform_identity::assign_project_role(system, alice.id(), "acme", "receiving", "admin")
        .await
        .expect_redacted("management-role negative control");
    refuse(https, alice_pat.token(), aud).await;
    grant(system, &alice, "acme", "dev").await;

    let response = https
        .client
        .get(format!("{}/.well-known/jwks.json", https.endpoint))
        .send()
        .await
        .expect_redacted("public JWKS HTTPS");
    assert_eq!(response.status(), 200);
    let bytes = response.bytes().await.expect_redacted("public JWKS body");
    let public: Value = serde_json::from_slice(&bytes).expect_redacted("JWKS JSON");
    assert_fields(&public, &["keys"]);
    for key in public["keys"].as_array().expect_redacted("public keys") {
        assert_fields(key, &["alg", "crv", "kid", "kty", "use", "x"]);
    }
    let jwks: SessionJwks =
        serde_json::from_slice(&bytes).expect_redacted("strict public-only keys");
    assert_eq!(jwks.keys.len(), 1);
    assert!(!String::from_utf8_lossy(&bytes).contains(PASSWORD));
    for target in &fixture.targets {
        assert!(
            reader_pids(system, target).await.is_empty(),
            "configured but unused targets must stay disconnected"
        );
    }

    let before = snapshots(fixture).await;
    let first = success(
        https,
        alice_pat.token(),
        &fixture.targets[0],
        &alice,
        &["receiver"],
        &jwks,
    )
    .await;
    let dev_pid = only_reader_pid(system, &fixture.targets[0]).await;
    let second = success(
        https,
        alice_pat.token(),
        &fixture.targets[0],
        &alice,
        &["receiver"],
        &jwks,
    )
    .await;
    assert!(
        first.jti != second.jti,
        "token identifiers are newly minted"
    );
    assert_eq!(
        only_reader_pid(system, &fixture.targets[0]).await,
        dev_pid,
        "sequential exchanges reuse the actual reader backend"
    );
    success(
        https,
        bob_pat.token(),
        &fixture.targets[0],
        &bob,
        &["auditor"],
        &jwks,
    )
    .await;
    assert!(
        before == snapshots(fixture).await,
        "exchanges must not insert, update, or delete any persisted fixture row"
    );
    let _ = tokio::join!(
        success(
            https,
            alice_pat.token(),
            &fixture.targets[0],
            &alice,
            &["receiver"],
            &jwks
        ),
        success(
            https,
            bob_pat.token(),
            &fixture.targets[0],
            &bob,
            &["auditor"],
            &jwks
        ),
    );
    assert_eq!(
        only_reader_pid(system, &fixture.targets[0]).await,
        dev_pid,
        "overlapping exchanges retain only the same single reader"
    );

    // Exact issuer, org, and audience are verified below with the public key.
    // Here the HTTP exchange itself must refuse membership in other targets.
    for target in &fixture.targets[1..] {
        refuse(https, alice_pat.token(), target.audience()).await;
        assert!(
            reader_pids(system, target).await.is_empty(),
            "membership refusal must not open an unused target"
        );
    }
    grant(system, &alice, "acme", "prod").await;
    success(
        https,
        alice_pat.token(),
        &fixture.targets[1],
        &alice,
        &["prod-reader"],
        &jwks,
    )
    .await;
    let prod_pid = only_reader_pid(system, &fixture.targets[1]).await;
    grant(system, &alice, "other", "dev").await;
    success(
        https,
        alice_pat.token(),
        &fixture.targets[2],
        &alice,
        &["other-reader"],
        &jwks,
    )
    .await;
    let other_pid = only_reader_pid(system, &fixture.targets[2]).await;
    assert!(
        BTreeSet::from([dev_pid, prod_pid, other_pid]).len() == 3,
        "each active target owns a distinct scoped reader backend"
    );
    refuse(
        https,
        alice_pat.token(),
        "urn:wamn:project-env:acme:receiving:dev:absent00",
    )
    .await;
    refuse(
        https,
        alice_pat.token(),
        "https://caller-selected.invalid/database",
    )
    .await;
    refuse(https, service_pat.token(), aud).await;

    assert!(
        revoke_project_env_membership(system, alice.id(), "acme", "receiving", "dev")
            .await
            .expect_redacted("revoke exact membership")
    );
    refuse(https, alice_pat.token(), aud).await;
    grant(system, &alice, "acme", "dev").await;
    success(
        https,
        alice_pat.token(),
        &fixture.targets[0],
        &alice,
        &["receiver"],
        &jwks,
    )
    .await;

    dev.execute(
        "DELETE FROM app_system.user_roles WHERE tenant_id=$1 AND user_id=$2::text::uuid",
        &[&TENANT, &alice.id().as_str()],
    )
    .await
    .expect_redacted("remove only target principal roles");
    // Alice's other-tenant row and Bob's current-tenant row both remain active.
    refuse(https, alice_pat.token(), aud).await;
    seed_user(dev, &alice, TENANT, "receiver").await;
    for status in ["disabled", "invited"] {
        dev.execute(
            "UPDATE app_system.users SET status=$1 WHERE tenant_id=$2 AND id=$3::text::uuid",
            &[&status, &TENANT, &alice.id().as_str()],
        )
        .await
        .expect_redacted("tenant user status control");
        refuse(https, alice_pat.token(), aud).await;
    }
    dev.execute(
        "DELETE FROM app_system.users WHERE tenant_id=$1 AND id=$2::text::uuid",
        &[&TENANT, &alice.id().as_str()],
    )
    .await
    .expect_redacted("remove target tenant user");
    refuse(https, alice_pat.token(), aud).await;
    seed_user(dev, &alice, TENANT, "receiver").await;

    // Same triple and old membership, different current physical incarnation.
    system.batch_execute("UPDATE registry.project_envs SET instance_suffix='n3w00002' WHERE org='acme' AND project='receiving' AND env='dev'")
        .await.expect_redacted("environment replacement control");
    refuse(https, alice_pat.token(), aud).await;
    system.execute("UPDATE registry.project_envs SET instance_suffix=$1 WHERE org='acme' AND project='receiving' AND env='dev'", &[&SUFFIX])
        .await.expect_redacted("restore current fixture incarnation");

    malformed_requests(https, alice_pat.token(), aud).await;
    original_validation_time_and_timeout(fixture, https, alice_pat.token(), &alice, &jwks).await;
    failed_role_queries_discard_reader(fixture, https, alice_pat.token(), &alice, &jwks).await;

    // Every fresh credential change must affect the very next exchange.
    disable_principal(system, alice.id())
        .await
        .expect_redacted("disable principal");
    refuse(https, alice_pat.token(), aud).await;
    system.execute("UPDATE identity.principals SET status='active', disabled_at=NULL WHERE id=$1::text::uuid", &[&alice.id().as_str()])
        .await.expect_redacted("restore fixture principal");
    let expired = issue_pat(
        system,
        alice.id(),
        "expired control",
        Duration::from_secs(3600),
    )
    .await
    .expect_redacted("expiring PAT");
    system.execute("UPDATE identity.pats SET created_at=clock_timestamp()-interval '2 hours', expires_at=clock_timestamp()-interval '1 hour' WHERE token_prefix=$1", &[&expired.record().prefix()])
        .await.expect_redacted("expired PAT control");
    refuse(https, expired.token(), aud).await;
    revoke_pat(system, alice_pat.record().prefix())
        .await
        .expect_redacted("revoke human PAT");
    refuse(https, alice_pat.token(), aud).await;
    success(
        https,
        bob_pat.token(),
        &fixture.targets[0],
        &bob,
        &["auditor"],
        &jwks,
    )
    .await;

    system
        .batch_execute(&format!(
            "REVOKE SELECT (token_hash) ON identity.pats FROM {}",
            quote_ident(IDENTITY_ISSUER_ROLE)
        ))
        .await
        .expect_redacted("backend permission failure control");
    let response = request(https, bob_pat.token(), aud)
        .send()
        .await
        .expect_redacted("redacted backend refusal");
    assert_failure(response, 503, "{\"error\":\"identity unavailable\"}").await;
    system
        .batch_execute(&grant_identity_issuer_surface_sql())
        .await
        .expect_redacted("restore exact issuer fixture surface");
    retained_successor_allows_real_retirement(fixture, https, bob_pat.token(), &bob, &jwks).await;
}

async fn malformed_requests(https: &Https, pat: &str, audience: &str) {
    for authorization in [
        None,
        Some("Basic absent"),
        Some("Bearer"),
        Some("Bearer forged"),
        Some("Bearer "),
    ] {
        let mut request = https
            .client
            .post(format!("{}/session", https.endpoint))
            .header("content-type", "application/json")
            .body(json!({"aud":audience}).to_string());
        if let Some(value) = authorization {
            request = request.header("authorization", value);
        }
        assert_failure(
            request
                .send()
                .await
                .expect_redacted("malformed authorization response"),
            401,
            "{\"error\":\"unauthorized\"}",
        )
        .await;
    }
    let duplicate = request(https, pat, audience).header("authorization", format!("Bearer {pat}"));
    assert_failure(
        duplicate
            .send()
            .await
            .expect_redacted("duplicate authorization response"),
        401,
        "{\"error\":\"unauthorized\"}",
    )
    .await;
    for body in [
        "{}".to_owned(),
        "not-json".to_owned(),
        "[]".to_owned(),
        format!("{{\"aud\":\"{audience}\",\"aud\":\"{audience}\"}}"),
        json!({"aud":audience,"roles":["admin"]}).to_string(),
        json!({"aud":audience,"principal":"caller-supplied"}).to_string(),
        json!({"aud":audience,"tenant":OTHER_TENANT}).to_string(),
        json!({"aud":audience,"database_url":"postgres://untrusted.invalid/db"}).to_string(),
        json!({"aud":"x".repeat(2048)}).to_string(),
    ] {
        assert_failure(
            request(https, pat, audience)
                .body(body)
                .send()
                .await
                .expect_redacted("malformed body response"),
            401,
            "{\"error\":\"unauthorized\"}",
        )
        .await;
    }
    let missing_type = https
        .client
        .post(format!("{}/session", https.endpoint))
        .bearer_auth(pat)
        .body(json!({"aud":audience}).to_string());
    assert_failure(
        missing_type
            .send()
            .await
            .expect_redacted("missing content type response"),
        401,
        "{\"error\":\"unauthorized\"}",
    )
    .await;
}

fn request(https: &Https, pat: &str, audience: &str) -> reqwest::RequestBuilder {
    https
        .client
        .post(format!("{}/session", https.endpoint))
        .bearer_auth(pat)
        .header("content-type", "application/json")
        .body(json!({"aud":audience}).to_string())
}

async fn refuse(https: &Https, pat: &str, audience: &str) {
    assert_failure(
        request(https, pat, audience)
            .send()
            .await
            .expect_redacted("fresh refusal response"),
        401,
        "{\"error\":\"unauthorized\"}",
    )
    .await;
}

async fn assert_failure(response: reqwest::Response, status: u16, body: &str) {
    assert_eq!(response.status().as_u16(), status);
    assert!(
        response.headers()["cache-control"] == "no-store",
        "refusal must not be cached"
    );
    assert!(
        response.headers()["content-type"] == "application/json",
        "refusal must be JSON"
    );
    if status == 401 {
        assert!(
            response.headers()["www-authenticate"] == "Bearer",
            "fixed bearer challenge"
        );
    }
    // Boolean equality avoids printing a token or key if a refusal regresses.
    assert!(
        response.text().await.expect_redacted("refusal body") == body,
        "fixed indistinguishable refusal body"
    );
}

async fn success(
    https: &Https,
    pat: &str,
    target: &SessionTarget,
    principal: &Principal,
    roles: &[&str],
    jwks: &SessionJwks,
) -> SessionClaims {
    let started = unix_seconds();
    let response = request(https, pat, target.audience())
        .send()
        .await
        .expect_redacted("exchange HTTPS response");
    let claims = verify_response(response, target, principal, roles, jwks).await;
    let returned = unix_seconds();
    assert!(claims.iat >= started && claims.iat <= returned);
    assert!(claims.exp >= started + MAXIMUM_LIFETIME && claims.exp <= returned + MAXIMUM_LIFETIME);
    claims
}

async fn verify_response(
    response: reqwest::Response,
    target: &SessionTarget,
    principal: &Principal,
    roles: &[&str],
    jwks: &SessionJwks,
) -> SessionClaims {
    assert_eq!(response.status(), 200);
    assert!(
        response.headers()["cache-control"] == "no-store",
        "exchange must not be cached"
    );
    assert!(
        response.headers()["content-type"] == "application/json",
        "exchange must be JSON"
    );
    let bytes = response.bytes().await.expect_redacted("exchange body");
    assert!(!String::from_utf8_lossy(&bytes).contains(PASSWORD));
    let body: Value = serde_json::from_slice(&bytes).expect_redacted("exchange JSON");
    assert_fields(&body, &["access_token", "expires_at", "token_type"]);
    assert!(body["token_type"] == "Bearer");
    let token = body["access_token"]
        .as_str()
        .expect_redacted("bearer string");
    let kid = session_key_id(token).expect_redacted("fixed signature profile");
    let key = jwks
        .keys
        .iter()
        .find(|key| key.kid == kid)
        .expect_redacted("key from actual public JWKS");
    let scope = SessionScope {
        issuer: ISSUER,
        org: &target.triple().org,
        audience: target.audience(),
    };
    let claims = verify_session_token(token, key, scope, unix_seconds())
        .expect_redacted("public signature and exact scope");
    assert!(
        claims.sub == principal.id().as_str(),
        "exact authenticated subject"
    );
    assert!(claims.roles == roles, "exact current tenant roles");
    assert_eq!(body["expires_at"].as_i64(), Some(claims.exp));
    for wrong in [
        SessionScope {
            issuer: "https://wrong-issuer.invalid",
            ..scope
        },
        SessionScope {
            org: "wrong-org",
            ..scope
        },
        SessionScope {
            audience: "wrong-audience",
            ..scope
        },
    ] {
        assert!(
            verify_session_token(token, key, wrong, unix_seconds()).is_err(),
            "signed claims cannot change configured scope"
        );
    }
    claims
}

async fn original_validation_time_and_timeout(
    fixture: &Fixture,
    https: &Https,
    pat: &str,
    principal: &Principal,
    jwks: &SessionJwks,
) {
    let system = &fixture.system.client;
    system
        .batch_execute("BEGIN; LOCK TABLE identity.principals IN ACCESS EXCLUSIVE MODE")
        .await
        .expect_redacted("hold fresh-auth barrier");
    let pending = request(https, pat, fixture.targets[0].audience());
    let mut response = tokio::spawn(async move { pending.send().await });
    wait_for_blocked_issuer(system, &fixture.issuer_role).await;
    let blocked_at = unix_seconds();
    // This timeout intentionally advances age, not readiness: pg_blocking_pids
    // already proved the request reached the real authentication read.
    assert!(
        tokio::time::timeout(Duration::from_secs(2), &mut response)
            .await
            .is_err(),
        "no token before fresh authentication completes"
    );
    let released_at = unix_seconds();
    system
        .batch_execute("ROLLBACK")
        .await
        .expect_redacted("release auth barrier");
    let response = response
        .await
        .expect_redacted("exchange task")
        .expect_redacted("delayed HTTPS response");
    let claims = verify_response(
        response,
        &fixture.targets[0],
        principal,
        &["receiver"],
        jwks,
    )
    .await;
    assert!(
        released_at >= blocked_at + 2,
        "barrier must span two clock seconds"
    );
    assert!(claims.iat >= released_at, "iat is actual signing time");
    assert!(
        claims.exp <= blocked_at + MAXIMUM_LIFETIME,
        "expiry stays anchored before the authentication stall, not at resume/sign time"
    );

    system
        .batch_execute("BEGIN; LOCK TABLE identity.principals IN ACCESS EXCLUSIVE MODE")
        .await
        .expect_redacted("hold timeout control");
    let pending = request(https, pat, fixture.targets[0].audience());
    let response = tokio::spawn(async move { pending.send().await });
    wait_for_blocked_issuer(system, &fixture.issuer_role).await;
    let response = tokio::time::timeout(Duration::from_secs(7), response)
        .await
        .expect_redacted("service deadline is bounded")
        .expect_redacted("timeout task")
        .expect_redacted("timeout HTTP response");
    system
        .batch_execute("ROLLBACK")
        .await
        .expect_redacted("release timed-out read");
    assert_failure(response, 503, "{\"error\":\"identity unavailable\"}").await;
    success(
        https,
        pat,
        &fixture.targets[0],
        principal,
        &["receiver"],
        jwks,
    )
    .await;
}

async fn wait_for_blocked_issuer(system: &Client, issuer_role: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            system.query_one("SELECT pg_stat_clear_snapshot()", &[]).await.expect_redacted("refresh backend observation");
            let blocked: bool = system.query_one("SELECT EXISTS (SELECT FROM pg_stat_activity WHERE usename=$1 AND pg_backend_pid()=ANY(pg_blocking_pids(pid)))", &[&issuer_role])
                .await.expect_redacted("observe actual issuer wait").get(0);
            if blocked { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect_redacted("issuer must reach the held PostgreSQL lock");
}

async fn reader_pids(observer: &Client, target: &SessionTarget) -> Vec<i32> {
    observer
        .query_one("SELECT pg_stat_clear_snapshot()", &[])
        .await
        .expect_redacted("refresh reader observation");
    observer
        .query(
            "SELECT pid FROM pg_stat_activity WHERE usename=$1 AND datname=$2 ORDER BY pid",
            &[&target.connection().role(), &target.connection().database()],
        )
        .await
        .expect_redacted("actual scoped reader backends")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

async fn only_reader_pid(observer: &Client, target: &SessionTarget) -> i32 {
    let pids = reader_pids(observer, target).await;
    assert_eq!(
        pids.len(),
        1,
        "an active target retains exactly one reader backend"
    );
    pids[0]
}

async fn wait_reader_closed(observer: &Client, target: &SessionTarget) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let pids = reader_pids(observer, target).await;
            assert!(
                pids.len() <= 1,
                "a failed target never owns multiple readers"
            );
            if pids.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect_redacted("discarded reader must disconnect");
}

async fn failed_role_queries_discard_reader(
    fixture: &Fixture,
    https: &Https,
    pat: &str,
    principal: &Principal,
    jwks: &SessionJwks,
) {
    let dev = &fixture.environments[0].client;
    let target = &fixture.targets[0];
    let before = only_reader_pid(dev, target).await;
    let prod_before = only_reader_pid(dev, &fixture.targets[1]).await;
    let other_before = only_reader_pid(dev, &fixture.targets[2]).await;
    dev.batch_execute("BEGIN; LOCK TABLE app_system.user_roles IN ACCESS EXCLUSIVE MODE")
        .await
        .expect_redacted("hold actual role read");
    let pending = request(https, pat, target.audience());
    let pending = tokio::spawn(async move { pending.send().await });
    wait_for_blocked_issuer(dev, target.connection().role()).await;
    assert_eq!(
        only_reader_pid(dev, target).await,
        before,
        "role read uses the retained scoped backend"
    );
    let response = tokio::time::timeout(Duration::from_secs(7), pending)
        .await
        .expect_redacted("role query deadline")
        .expect_redacted("role query task")
        .expect_redacted("role query timeout response");
    dev.batch_execute("ROLLBACK")
        .await
        .expect_redacted("release canceled role query");
    assert_failure(response, 503, "{\"error\":\"identity unavailable\"}").await;
    wait_reader_closed(dev, target).await;
    success(https, pat, target, principal, &["receiver"], jwks).await;
    let replacement = only_reader_pid(dev, target).await;
    assert_ne!(
        before, replacement,
        "a canceled role read is never returned to its slot"
    );

    dev.batch_execute(&format!(
        "REVOKE SELECT (role_name) ON app_system.user_roles FROM {}",
        quote_ident(WorkloadRoleFamily::SessionRoleReader.acl_role())
    ))
    .await
    .expect_redacted("role query permission failure control");
    let response = request(https, pat, target.audience())
        .send()
        .await
        .expect_redacted("failed role read response");
    assert_failure(response, 503, "{\"error\":\"identity unavailable\"}").await;
    wait_reader_closed(dev, target).await;
    dev.batch_execute(&grant_session_role_reader_surface_sql())
        .await
        .expect_redacted("restore exact reader fixture grants");
    success(https, pat, target, principal, &["receiver"], jwks).await;
    assert_ne!(
        only_reader_pid(dev, target).await,
        replacement,
        "an errored role read reconnects on the next exchange"
    );
    assert_eq!(
        only_reader_pid(dev, &fixture.targets[1]).await,
        prod_before,
        "dev query failures do not replace the prod reader"
    );
    assert_eq!(
        only_reader_pid(dev, &fixture.targets[2]).await,
        other_before,
        "dev query failures do not replace another organization's reader"
    );
}

async fn retained_successor_allows_real_retirement(
    fixture: &Fixture,
    predecessor: &Https,
    pat: &str,
    principal: &Principal,
    jwks: &SessionJwks,
) {
    let target = &fixture.targets[0];
    let triple = target.triple();
    let dev = &fixture.environments[0].client;
    let predecessor_pid = only_reader_pid(dev, target).await;
    let successor_role = workload_generation_role(
        WorkloadRoleFamily::SessionRoleReader,
        WorkloadRoleScope::ProjectEnvironment {
            org: &triple.org,
            project: &triple.project,
            environment: triple.env.as_str(),
            database: target.connection().database(),
        },
        CredentialGeneration::B,
    )
    .expect_redacted("exact successor role");
    let absent: bool = dev
        .query_one(
            "SELECT NOT EXISTS (SELECT FROM pg_roles WHERE rolname=$1)",
            &[&successor_role],
        )
        .await
        .expect_redacted("successor fixture role must be absent")
        .get(0);
    assert!(absent, "never overwrite an existing successor role");
    dev.batch_execute(&prepare_workload_generation_sql(
        WorkloadRoleFamily::SessionRoleReader,
        target.connection().database(),
        &successor_role,
        PASSWORD,
        "2100-01-01T00:00:00Z",
    ))
    .await
    .expect_redacted("prepare exact successor through production builder");
    let admin =
        std::env::var("WAMN_SESSION_EXCHANGE_PG_URL").expect_redacted("armed administrator URL");
    let parsed = url::Url::parse(&admin).expect_redacted("administrator URL shape");
    let successor_target = SessionTarget::new(
        triple,
        target.instance_suffix(),
        target.tenant_id(),
        &login_url(&parsed, target.connection().database(), &successor_role),
    )
    .expect_redacted("successor target capability");
    let successor = start_with_targets(fixture, vec![successor_target.clone()]).await;
    assert!(
        reader_pids(dev, &successor_target).await.is_empty(),
        "starting the successor service alone opens no reader"
    );

    // No synthetic B connection is created anywhere in this fixture. The
    // existing CLI must refuse retirement until an actual B exchange occurs.
    let refusal = retire_reader_cli(&admin, target).await;
    assert!(
        !refusal.status.success(),
        "unopened successor cannot authorize retirement"
    );
    assert!(
        String::from_utf8_lossy(&refusal.stderr).contains("no verified live private-pool session"),
        "retirement reached the actual successor-session guard"
    );
    assert_eq!(
        only_reader_pid(dev, target).await,
        predecessor_pid,
        "refused retirement keeps A connected"
    );
    success(
        &successor,
        pat,
        &successor_target,
        principal,
        &["auditor"],
        jwks,
    )
    .await;
    let successor_pid = only_reader_pid(dev, &successor_target).await;
    let retirement = retire_reader_cli(&admin, target).await;
    assert!(
        retirement.status.success(),
        "real retained B service session permits existing CLI retirement"
    );
    wait_reader_closed(dev, target).await;
    let inactive: bool = dev
        .query_one(
            "SELECT NOT rolcanlogin FROM pg_roles WHERE rolname=$1",
            &[&target.connection().role()],
        )
        .await
        .expect_redacted("retired reader login state")
        .get(0);
    assert!(inactive, "retired A generation cannot reconnect");
    assert_eq!(
        only_reader_pid(dev, &successor_target).await,
        successor_pid,
        "retirement preserves the actual B service backend"
    );
    let response = request(predecessor, pat, target.audience())
        .send()
        .await
        .expect_redacted("retired predecessor response");
    assert_failure(response, 503, "{\"error\":\"identity unavailable\"}").await;
    success(
        &successor,
        pat,
        &successor_target,
        principal,
        &["auditor"],
        jwks,
    )
    .await;
    assert_eq!(
        only_reader_pid(dev, &successor_target).await,
        successor_pid,
        "successor keeps serving on the retained backend"
    );
    drop(successor);
    wait_reader_closed(dev, &successor_target).await;
    dev.batch_execute(&format!(
        "DROP OWNED BY {role}; DROP ROLE {role}",
        role = quote_ident(&successor_role)
    ))
    .await
    .expect_redacted("remove exact successor fixture role");
}

async fn retire_reader_cli(admin: &str, target: &SessionTarget) -> std::process::Output {
    let binary = std::env::var_os("WAMN_SESSION_EXCHANGE_CTL_BIN")
        .expect_redacted("provide this worktree's compiled wamn-ctl binary");
    let binary = std::path::PathBuf::from(binary)
        .canonicalize()
        .expect_redacted("resolve compiled ctl binary");
    let expected = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/wamn-ctl")
        .canonicalize()
        .expect_redacted("this worktree's default-target ctl binary");
    assert!(
        binary == expected,
        "retirement test must use this worktree's compiled ctl"
    );
    let mut project = url::Url::parse(admin).expect_redacted("administrator URL shape");
    project.set_path(&format!("/{}", target.connection().database()));
    let triple = target.triple();
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(binary)
            .args([
                "provision-project-env",
                "--org",
                &triple.org,
                "--project",
                &triple.project,
                "--env",
                triple.env.as_str(),
                "--tenant",
                target.tenant_id(),
                "--retire-session-role-reader-generation",
                "a",
            ])
            .env_remove("WAMN_APP_PASSWORD")
            .env("WAMN_SYSTEM_ADMIN_URL", admin)
            .env("WAMN_TARGET_ADMIN_DATABASE_URL", project.as_str())
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect_redacted("bounded actual retirement CLI")
    .expect_redacted("run compiled retirement CLI");
    for secret in [
        Some(PASSWORD),
        Some(admin),
        Some(project.as_str()),
        project.password(),
    ]
    .into_iter()
    .flatten()
    {
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains(secret)
                && !String::from_utf8_lossy(&output.stderr).contains(secret),
            "retirement diagnostics must not disclose credentials"
        );
    }
    output
}

async fn snapshots(fixture: &Fixture) -> Vec<(String, Vec<String>)> {
    let mut snapshots = Vec::new();
    for (index, database) in std::iter::once(&fixture.system)
        .chain(&fixture.environments)
        .enumerate()
    {
        let tables = database.client.query("SELECT n.nspname, c.relname FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN ('identity','registry','provisioning','app_system','catalog','wamn_run','wamn_authority') AND c.relkind='r' ORDER BY n.nspname,c.relname", &[])
            .await.expect_redacted("fixture relation inventory");
        for table in tables {
            let schema: String = table.get(0);
            let name: String = table.get(1);
            let versions: Vec<String> = database.client.query_one(&format!("SELECT coalesce(array_agg(md5(row_to_json(t)::text)||':'||t.xmin::text||':'||t.ctid::text ORDER BY t.ctid),ARRAY[]::text[]) FROM {}.{} t", quote_ident(&schema), quote_ident(&name)), &[])
                .await.expect_redacted("server-side opaque row-version snapshot").get(0);
            snapshots.push((format!("{index}:{schema}.{name}"), versions));
        }
    }
    snapshots
}

async fn seed_user(client: &Client, principal: &Principal, tenant: &str, role: &str) {
    client.execute("INSERT INTO app_system.users (tenant_id,id,email) VALUES ($1,$2::text::uuid,$3) ON CONFLICT (tenant_id,id) DO UPDATE SET status='active'", &[&tenant, &principal.id().as_str(), &format!("{}@fixture.invalid", principal.id())])
        .await.expect_redacted("tenant user fixture");
    client
        .execute(
            "INSERT INTO app_system.roles (tenant_id,name) VALUES ($1,$2) ON CONFLICT DO NOTHING",
            &[&tenant, &role],
        )
        .await
        .expect_redacted("tenant role fixture");
    client.execute("INSERT INTO app_system.user_roles (tenant_id,user_id,role_name) VALUES ($1,$2::text::uuid,$3) ON CONFLICT DO NOTHING", &[&tenant, &principal.id().as_str(), &role])
        .await.expect_redacted("exact tenant/principal role fixture");
}

async fn grant(system: &Client, principal: &Principal, org: &str, env: &str) {
    grant_project_env_membership(system, principal.id(), org, "receiving", env)
        .await
        .expect_redacted("explicit environment grant");
}

async fn setup() -> Fixture {
    assert!(
        std::env::var("WAMN_SESSION_EXCHANGE_ALLOW_SCHEMA_RESET").as_deref() == Ok("1"),
        "arm only an owned disposable cluster"
    );
    let raw = std::env::var("WAMN_SESSION_EXCHANGE_PG_URL")
        .expect_redacted("provide disposable wamn_system URL");
    let admin = url::Url::parse(&raw).expect_redacted("armed URL shape");
    assert!(
        admin.path() == "/wamn_system",
        "never reset another database"
    );
    let system = connect(&raw).await;
    let safe: bool = system.client.query_one("SELECT current_database()='wamn_system' AND current_setting('server_version_num')::int BETWEEN 180000 AND 189999 AND rolsuper FROM pg_roles WHERE rolname=current_user", &[])
        .await.expect_redacted("disposable PG18 preflight").get(0);
    assert!(safe, "test needs dedicated PostgreSQL 18 superuser setup");
    let issuer_role = identity_issuer_generation_role(ISSUER, CredentialGeneration::A)
        .expect_redacted("issuer role");
    let mut roles = vec![
        issuer_role.clone(),
        IDENTITY_ISSUER_ROLE.to_owned(),
        WorkloadRoleFamily::SessionRoleReader.acl_role().to_owned(),
    ];
    roles.extend(
        [
            "wamn_app",
            "wamn_scenario_author",
            "wamn_control_author",
            "wamn_effect_writer",
            "wamn_platform",
            "wamn_run_retention",
        ]
        .map(str::to_owned),
    );
    let mut targets = Vec::new();
    for (org, env) in SCOPES {
        let triple = Triple {
            org: org.into(),
            project: "receiving".into(),
            env: env.into(),
        };
        let name = project_env_database_name(org, "receiving", env, SUFFIX);
        let role = workload_generation_role(
            WorkloadRoleFamily::SessionRoleReader,
            WorkloadRoleScope::ProjectEnvironment {
                org,
                project: "receiving",
                environment: env,
                database: &name,
            },
            CredentialGeneration::A,
        )
        .expect_redacted("reader role");
        let target = SessionTarget::new(&triple, SUFFIX, TENANT, &login_url(&admin, &name, &role))
            .expect_redacted("production target capability");
        assert!(!format!("{target:?}").contains(PASSWORD));
        roles.push(role);
        targets.push(target);
    }
    let databases: Vec<String> = targets
        .iter()
        .map(|t| t.connection().database().to_owned())
        .collect();
    let exists: bool = system.client.query_one("SELECT EXISTS (SELECT FROM pg_roles WHERE rolname=ANY($1::text[])) OR EXISTS (SELECT FROM pg_database WHERE datname=ANY($2::text[]))", &[&roles, &databases])
        .await.expect_redacted("exact fixture names must be absent").get(0);
    assert!(
        !exists,
        "refuse to overwrite existing fixture databases or authority roles"
    );
    system.client.batch_execute("DROP SCHEMA IF EXISTS identity CASCADE; DROP SCHEMA IF EXISTS provisioning CASCADE; DROP SCHEMA IF EXISTS registry CASCADE; DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN CREATE ROLE wamn_system; END IF; END $$; GRANT CREATE ON DATABASE wamn_system TO wamn_system; SET ROLE wamn_system;")
        .await.expect_redacted("prepare armed disposable system owner");
    system
        .client
        .batch_execute(include_str!("../../../deploy/sql/system-schema.sql"))
        .await
        .expect_redacted("production system schema");
    system.client.batch_execute("RESET ROLE; CREATE ROLE wamn_app NOLOGIN; CREATE ROLE wamn_scenario_author NOLOGIN; CREATE ROLE wamn_control_author NOLOGIN; CREATE ROLE wamn_effect_writer NOLOGIN;")
        .await.expect_redacted("schema prerequisite roles");
    for name in &databases {
        system
            .client
            .batch_execute(&format!("CREATE DATABASE {}", quote_ident(name)))
            .await
            .expect_redacted("create exact disposable environment");
    }
    system
        .client
        .batch_execute(revoke_public_connect_floor_sql())
        .await
        .expect_redacted("production CONNECT floor on disposable cluster");
    system
        .client
        .batch_execute("REVOKE TEMPORARY ON DATABASE wamn_system FROM PUBLIC")
        .await
        .expect_redacted("system TEMP floor");
    system
        .client
        .batch_execute(
            &prepare_identity_issuer_generation_sql(
                ISSUER,
                CredentialGeneration::A,
                PASSWORD,
                "2100-01-01T00:00:00Z",
            )
            .expect_redacted("issuer credential SQL"),
        )
        .await
        .expect_redacted("actual issuer provisioning");
    let issuer_url = login_url(&admin, "wamn_system", &issuer_role);
    let mut key_authority = connect(&issuer_url).await;
    let public = publish_session_key(&mut key_authority.client, ISSUER)
        .await
        .expect_redacted("publish through provisioned issuer");
    activate_session_key(&mut key_authority.client, ISSUER, &public.kid)
        .await
        .expect_redacted("activate through provisioned issuer");
    drop(key_authority);
    let mut environments = Vec::new();
    for target in &targets {
        let mut url = admin.clone();
        url.set_path(&format!("/{}", target.connection().database()));
        let environment = connect(url.as_str()).await;
        for schema in [
            wamn_catalog::CATALOG_SCHEMA_SQL,
            include_str!("../../../deploy/sql/run-state.sql"),
            include_str!("../../../deploy/sql/run-queue.sql"),
            include_str!("../../../deploy/sql/app-schema.sql"),
        ] {
            environment
                .client
                .batch_execute(schema)
                .await
                .expect_redacted("production environment schema");
        }
        environment
            .client
            .batch_execute(&format!(
                "REVOKE TEMPORARY ON DATABASE {} FROM PUBLIC",
                quote_ident(target.connection().database())
            ))
            .await
            .expect_redacted("environment TEMP floor");
        environment
            .client
            .batch_execute(&prepare_workload_generation_sql(
                WorkloadRoleFamily::SessionRoleReader,
                target.connection().database(),
                target.connection().role(),
                PASSWORD,
                "2100-01-01T00:00:00Z",
            ))
            .await
            .expect_redacted("actual dedicated role-reader provisioning");
        let triple = target.triple();
        system.client.execute("INSERT INTO registry.orgs (id,placement_kind) VALUES ($1,'dedicated') ON CONFLICT DO NOTHING", &[&triple.org]).await.expect_redacted("fixture org");
        system.client.execute("INSERT INTO registry.projects (org,id) VALUES ($1,'receiving') ON CONFLICT DO NOTHING", &[&triple.org]).await.expect_redacted("fixture project");
        system.client.execute("INSERT INTO registry.env_policies (org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) VALUES ($1,$2,'\"own\"',0,1,'1Gi','1','1Gi','fixture')", &[&triple.org, &triple.env.as_str()]).await.expect_redacted("fixture environment policy");
        system.client.execute("INSERT INTO registry.project_envs (org,project,env,secret_name,instance_suffix) VALUES ($1,'receiving',$2,'fixture-reference',$3)", &[&triple.org, &triple.env.as_str(), &SUFFIX]).await.expect_redacted("current environment registry entry");
        environments.push(environment);
    }
    Fixture {
        system,
        environments,
        targets,
        issuer_url,
        issuer_role,
        roles,
    }
}

async fn cleanup(fixture: Fixture) {
    let Fixture {
        system,
        environments,
        targets,
        roles,
        ..
    } = fixture;
    drop(environments);
    for target in targets {
        system
            .client
            .batch_execute(&format!(
                "DROP DATABASE {} WITH (FORCE)",
                quote_ident(target.connection().database())
            ))
            .await
            .expect_redacted("remove only owned disposable environment");
    }
    system.client.batch_execute("DROP SCHEMA identity CASCADE; DROP SCHEMA provisioning CASCADE; DROP SCHEMA registry CASCADE").await.expect_redacted("remove owned system fixture schemas");
    for role in roles {
        system
            .client
            .batch_execute(&format!(
                "DROP OWNED BY {role}; DROP ROLE {role}",
                role = quote_ident(&role)
            ))
            .await
            .expect_redacted("remove exact fixture authority role");
    }
}

async fn start(fixture: &Fixture, enabled: bool) -> Https {
    start_with_targets(
        fixture,
        if enabled {
            fixture.targets.clone()
        } else {
            Vec::new()
        },
    )
    .await
}

async fn start_with_targets(fixture: &Fixture, targets: Vec<SessionTarget>) -> Https {
    let config = IdentityConfig::new(ISSUER, &fixture.issuer_url)
        .expect_redacted("validated issuer configuration");
    let config = config
        .with_session_targets(targets)
        .expect_redacted("provisioned targets");
    assert!(!format!("{config:?}").contains(PASSWORD));
    let service = IdentityService::connect(config)
        .await
        .expect_redacted("production scoped service");
    assert!(!format!("{service:?}").contains(PASSWORD));
    let (certificate, private, ca) = certificates();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect_redacted("HTTPS listener");
    let endpoint = format!(
        "https://{}",
        listener.local_addr().expect_redacted("listener address")
    );
    let serving = tokio::spawn(serve(
        listener,
        service,
        tls_config(&certificate, &private).expect_redacted("production TLS"),
    ));
    let client = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .tls_backend_rustls()
        .tls_certs_only(reqwest::Certificate::from_pem_bundle(&ca).expect_redacted("fixture CA"))
        .build()
        .expect_redacted("trusted HTTPS client");
    Https {
        endpoint,
        client,
        serving,
    }
}

async fn connect(url: &str) -> Database {
    let (client, connection) =
        tokio::time::timeout(Duration::from_secs(5), tokio_postgres::connect(url, NoTls))
            .await
            .expect_redacted("bounded disposable connection")
            .map_err(|_| ())
            .expect_redacted("connect without credential diagnostics");
    Database {
        client,
        driver: tokio::spawn(async move {
            let _ = connection.await;
        }),
    }
}

fn login_url(admin: &url::Url, database: &str, role: &str) -> String {
    let mut url = admin.clone();
    url.set_path(&format!("/{database}"));
    url.set_username(role).expect_redacted("scoped role");
    url.set_password(Some(PASSWORD))
        .expect_redacted("fixture password");
    url.set_query(None);
    url.set_fragment(None);
    url.into()
}

fn assert_fields(value: &Value, fields: &[&str]) {
    assert!(
        value
            .as_object()
            .expect_redacted("public JSON object")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == fields.iter().copied().collect::<BTreeSet<_>>(),
        "exact public JSON field names"
    );
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect_redacted("clock after epoch")
        .as_secs()
        .try_into()
        .expect_redacted("Unix seconds fit")
}

fn certificates() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut ca_params =
        CertificateParams::new(Vec::<String>::new()).expect_redacted("CA parameters");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let ca_key = KeyPair::generate().expect_redacted("CA key");
    let ca = ca_params
        .self_signed(&ca_key)
        .expect_redacted("CA certificate");
    let issuer = Issuer::new(ca_params, ca_key);
    let key = KeyPair::generate().expect_redacted("TLS key");
    let certificate = CertificateParams::new(vec!["127.0.0.1".into()])
        .expect_redacted("TLS parameters")
        .signed_by(&key, &issuer)
        .expect_redacted("TLS certificate");
    (
        certificate.pem().into_bytes(),
        key.serialize_pem().into_bytes(),
        ca.pem().into_bytes(),
    )
}
