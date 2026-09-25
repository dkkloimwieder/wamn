//! HTTPS lifecycle checks share the exchange suite's owned database and TLS fixtures.

use super::*;
use sha2::{Digest as _, Sha256};
use wamn_platform_identity::password::{
    Password, enroll_password, issue_invitation, password_work,
};

const EMAIL: &str = "renewal-alice@example.invalid";

async fn enroll(fixture: &Fixture, email: &str, password: &str) -> Principal {
    let person = create_human(&fixture.system.client, email, email, "Renewal person")
        .await
        .expect_redacted("human");
    seed_user(&fixture.environments[0].client, &person, TENANT, "receiver").await;
    grant(&fixture.system.client, &person, "demo", "dev").await;
    let actor = PlatformComponent::Provisioning
        .principal_id()
        .to_string()
        .parse()
        .unwrap();
    let mut issuer = connect(&fixture.issuer_url).await;
    let invitation = issue_invitation(&mut issuer.client, &actor, person.id())
        .await
        .expect_redacted("invitation");
    enroll_password(
        &mut issuer.client,
        &password_work(),
        person.id(),
        invitation.secret(),
        Password::new(password.to_owned()).unwrap(),
    )
    .await
    .expect_redacted("enrollment");
    person
}
async fn server(fixture: &Fixture) -> Https {
    start_config(
        IdentityConfig::new(ISSUER, &fixture.issuer_url)
            .unwrap()
            .with_session_targets(fixture.targets.clone())
            .unwrap()
            .with_resend(
                wamn_identity::mail::ResendConfig::new(
                    "fixture-key".into(),
                    "WAMN <fixture@example.invalid>".into(),
                )
                .unwrap(),
            ),
    )
    .await
}
fn post(https: &Https, path: &str, body: &Value) -> reqwest::RequestBuilder {
    https
        .client
        .post(format!("{}{path}", https.endpoint))
        .json(body)
}
fn request(https: &Https, fixture: &Fixture, path: &str, token: &Value) -> reqwest::RequestBuilder {
    post(
        https,
        path,
        &json!({"aud":fixture.targets[0].audience(),"renewal_token":token["renewal_token"]}),
    )
}
async fn body(response: reqwest::Response) -> Value {
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let bytes = response.bytes().await.expect_redacted("response bytes");
    assert!(!String::from_utf8_lossy(&bytes).contains(PASSWORD));
    let result: Value = serde_json::from_slice(&bytes).expect_redacted("response JSON");
    assert_fields(
        &result,
        &[
            "access_token",
            "token_type",
            "expires_at",
            "renewal_token",
            "login_expires_at",
        ],
    );
    assert_eq!(result["token_type"], "Bearer");
    assert!(
        result["renewal_token"]
            .as_str()
            .is_some_and(|s| s.starts_with("wamn_renew_"))
    );
    result
}
async fn login(https: &Https, fixture: &Fixture) -> Value {
    discovery_window(fixture).await;
    body(
        post(
            https,
            "/password/session",
            &json!({"email":EMAIL,"password":PASSWORD,"aud":fixture.targets[0].audience()}),
        )
        .send()
        .await
        .expect_redacted("password login"),
    )
    .await
}
async fn claims(fixture: &Fixture, body: &Value) -> SessionClaims {
    let jwks = wamn_platform_identity::session_keys::session_jwks(&fixture.system.client, ISSUER)
        .await
        .expect_redacted("JWKS");
    let token = body["access_token"].as_str().expect_redacted("token");
    let kid = session_key_id(token).expect_redacted("kid");
    let key = jwks
        .keys
        .iter()
        .find(|key| key.kid == kid)
        .expect_redacted("public key");
    verify_session_token(
        token,
        key,
        SessionScope {
            issuer: ISSUER,
            org: "demo",
            audience: fixture.targets[0].audience(),
        },
        unix_seconds(),
    )
    .expect_redacted("signed session")
}

async fn active(fixture: &Fixture, body: &Value) -> bool {
    wamn_platform_identity::session_token::session_is_active(
        &fixture.system.client,
        &claims(fixture, body).await,
        "widgets",
        "dev",
    )
    .await
    .expect_redacted("current session authority")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn renewal_rotation_authority_expiry_and_logout_use_real_https() {
    let mut postgres = wamn_test_postgres::start(&[]).expect_redacted("owned PostgreSQL");
    let db = postgres
        .create_database("wamn_system")
        .expect_redacted("owned system database");
    let fixture = setup(db.url()).await;
    let person = enroll(&fixture, EMAIL, PASSWORD).await;
    let https = server(&fixture).await;
    let first = login(&https, &fixture).await;
    let verified = claims(&fixture, &first).await;
    assert_eq!(verified.sub, person.id().as_str());
    assert_eq!(verified.roles, vec!["receiver"]);
    assert!(active(&fixture, &first).await);
    let second = body(
        request(&https, &fixture, "/password/renew", &first)
            .send()
            .await
            .expect_redacted("renewal"),
    )
    .await;
    #[expect(
        clippy::manual_assert_eq,
        reason = "never print renewal credentials on failure"
    )]
    {
        assert!(first["renewal_token"] != second["renewal_token"]);
    }
    assert_eq!(first["login_expires_at"], second["login_expires_at"]);
    assert_failure(
        request(&https, &fixture, "/password/renew", &first)
            .send()
            .await
            .unwrap(),
        401,
        "{\"error\":\"unauthorized\"}",
    )
    .await;
    assert_failure(
        request(&https, &fixture, "/password/renew", &second)
            .send()
            .await
            .unwrap(),
        401,
        "{\"error\":\"unauthorized\"}",
    )
    .await;
    // A valid signature cannot bypass a revoked login.
    assert!(!active(&fixture, &first).await);
    assert!(!active(&fixture, &second).await);

    let active = login(&https, &fixture).await;
    let wrong = post(
        &https,
        "/password/renew",
        &json!({"aud":"wrong","renewal_token":active["renewal_token"]}),
    )
    .send()
    .await
    .unwrap();
    assert_failure(wrong, 401, "{\"error\":\"unauthorized\"}").await;
    revoke_project_env_membership(
        &fixture.system.client,
        person.id(),
        "demo",
        "widgets",
        "dev",
    )
    .await
    .unwrap();
    assert_eq!(
        request(&https, &fixture, "/password/renew", &active)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    grant(&fixture.system.client, &person, "demo", "dev").await;
    fixture.environments[0]
        .client
        .execute(
            "UPDATE app_system.users SET status='disabled' WHERE id=$1::text::uuid",
            &[&person.id().as_str()],
        )
        .await
        .unwrap();
    assert_eq!(
        request(&https, &fixture, "/password/renew", &active)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    fixture.environments[0]
        .client
        .execute(
            "UPDATE app_system.users SET status='active' WHERE id=$1::text::uuid",
            &[&person.id().as_str()],
        )
        .await
        .unwrap();
    fixture.environments[0]
        .client
        .execute(
            "DELETE FROM app_system.user_roles WHERE user_id=$1::text::uuid",
            &[&person.id().as_str()],
        )
        .await
        .unwrap();
    assert_eq!(
        request(&https, &fixture, "/password/renew", &active)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    fixture.environments[0].client.execute("INSERT INTO app_system.user_roles (tenant_id,user_id,role_name) VALUES ($1,$2::text::uuid,'receiver')",&[&TENANT,&person.id().as_str()]).await.unwrap();
    fixture.system.client.batch_execute("UPDATE registry.project_envs SET instance_suffix='replaced' WHERE org='demo' AND env='dev'").await.unwrap();
    assert_eq!(
        request(&https, &fixture, "/password/renew", &active)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    fixture.system.client.batch_execute("UPDATE registry.project_envs SET instance_suffix='s3ss10n2' WHERE org='demo' AND env='dev'").await.unwrap();
    let active = body(
        request(&https, &fixture, "/password/renew", &active)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        request(&https, &fixture, "/password/logout", &active)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert_eq!(
        request(&https, &fixture, "/password/logout", &active)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert_eq!(
        request(&https, &fixture, "/password/renew", &active)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert!(!self::active(&fixture, &active).await);

    let a = login(&https, &fixture).await;
    let b = login(&https, &fixture).await;
    assert_eq!(
        request(&https, &fixture, "/password/logout-all", &a)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert!(!self::active(&fixture, &a).await);
    assert!(!self::active(&fixture, &b).await);
    assert_eq!(
        request(&https, &fixture, "/password/renew", &b)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let near_expiry = login(&https, &fixture).await;
    fixture.system.client.execute("WITH deadline AS (SELECT date_trunc('second',clock_timestamp())+interval '60 seconds' AS at) UPDATE identity.password_logins SET authenticated_at=deadline.at-interval '8 hours',expires_at=deadline.at,renewal_expires_at=deadline.at FROM deadline WHERE principal_id=$1::text::uuid AND revoked_at IS NULL",&[&person.id().as_str()]).await.unwrap();
    let bounded = body(
        request(&https, &fixture, "/password/renew", &near_expiry)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(bounded["expires_at"], bounded["login_expires_at"]);
    assert!(claims(&fixture, &bounded).await.exp <= unix_seconds() + 60);
    fixture.system.client.execute("UPDATE identity.password_logins SET authenticated_at=authenticated_at-interval '9 hours',expires_at=expires_at-interval '9 hours',renewal_expires_at=renewal_expires_at-interval '9 hours' WHERE principal_id=$1::text::uuid",&[&person.id().as_str()]).await.unwrap();
    assert_eq!(
        request(&https, &fixture, "/password/renew", &bounded)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        fixture
            .system
            .client
            .query_one("SELECT count(*) FROM identity.password_logins", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let disabled = login(&https, &fixture).await;
    assert!(self::active(&fixture, &disabled).await);
    disable_principal(&fixture.system.client, person.id())
        .await
        .unwrap();
    assert!(!self::active(&fixture, &disabled).await);
    assert_eq!(
        request(&https, &fixture, "/password/renew", &disabled)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    drop(https);
    cleanup(fixture).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn login_reset_and_renewal_logout_races_keep_transaction_order() {
    let mut postgres = wamn_test_postgres::start(&[]).expect_redacted("owned PostgreSQL");
    let db = postgres
        .create_database("wamn_system")
        .expect_redacted("owned system database");
    let fixture = setup(db.url()).await;
    let person = enroll(&fixture, EMAIL, PASSWORD).await;
    let https = server(&fixture).await;
    let first = login(&https, &fixture).await;
    fixture
        .system
        .client
        .batch_execute("BEGIN; SELECT issuer FROM identity.session_signing_state FOR UPDATE")
        .await
        .unwrap();
    let pending = request(&https, &fixture, "/password/renew", &first);
    let renewal = tokio::spawn(async move { pending.send().await });
    wait_for_blocked_issuer(&fixture.system.client, &fixture.issuer_role).await;
    let pending = request(&https, &fixture, "/password/logout-all", &first);
    let logout = tokio::spawn(async move { pending.send().await });
    tokio::time::timeout(HANG_GUARD,async{
        loop {
            fixture.system.client.query_one("SELECT pg_stat_clear_snapshot()",&[]).await.unwrap();
            let waiting:i64=fixture.system.client.query_one("SELECT count(*) FROM pg_stat_activity WHERE usename=$1 AND cardinality(pg_blocking_pids(pid))>0",&[&fixture.issuer_role]).await.unwrap().get(0);
            if waiting>=2 {break}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect_redacted("renewal and logout reach their locks");
    fixture.system.client.batch_execute("COMMIT").await.unwrap();
    let replacement = body(renewal.await.unwrap().unwrap()).await;
    // Logout presented the now-consumed credential. Its replay revokes the family.
    assert_eq!(logout.await.unwrap().unwrap().status(), 401);
    assert_eq!(
        request(&https, &fixture, "/password/renew", &replacement)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );

    let before_reset = login(&https, &fixture).await;
    let mut reset_connection = connect(&fixture.issuer_url).await;
    let actor = PlatformComponent::Provisioning
        .principal_id()
        .to_string()
        .parse()
        .unwrap();
    let reset_secret = wamn_platform_identity::password::issue_reset(
        &mut reset_connection.client,
        &actor,
        person.id(),
        EMAIL,
    )
    .await
    .unwrap();
    discovery_window(&fixture).await;
    fixture.system.client.batch_execute("BEGIN").await.unwrap();
    fixture
        .system
        .client
        .query_one(
            "SELECT id FROM identity.principals WHERE id=$1::text::uuid FOR UPDATE",
            &[&person.id().as_str()],
        )
        .await
        .unwrap();
    let reset_principal = person.id().clone();
    let reset = tokio::spawn(async move {
        wamn_platform_identity::password::reset_password(
            &mut reset_connection.client,
            &password_work(),
            &reset_principal,
            reset_secret.secret(),
            Password::new("a different valid replacement password".into()).unwrap(),
        )
        .await
    });
    wait_for_blocked_issuer(&fixture.system.client, &fixture.issuer_role).await;
    let pending = post(
        &https,
        "/password/session",
        &json!({"email":EMAIL,"password":PASSWORD,"aud":fixture.targets[0].audience()}),
    );
    let old_login = tokio::spawn(async move { pending.send().await });
    let pending_renewal = request(&https, &fixture, "/password/renew", &before_reset);
    let reset_racing_renewal = tokio::spawn(async move { pending_renewal.send().await });
    wait_for_blocked_issuer(&fixture.system.client, &fixture.issuer_role).await;
    fixture.system.client.batch_execute("COMMIT").await.unwrap();
    reset.await.unwrap().unwrap();
    assert!(!active(&fixture, &before_reset).await);
    assert_eq!(reset_racing_renewal.await.unwrap().unwrap().status(), 401);
    assert_failure(
        old_login.await.unwrap().unwrap(),
        401,
        "{\"error\":\"unauthorized\"}",
    )
    .await;
    assert_eq!(fixture.system.client.query_one("SELECT count(*) FROM identity.password_logins WHERE principal_id=$1::text::uuid AND revoked_at IS NULL",&[&person.id().as_str()]).await.unwrap().get::<_,i64>(0),0);
    drop(https);
    cleanup(fixture).await;
}

const COOKIES: [(&str, &str, &str); 3] = [
    ("__Host-wamn-session", "/", "; HttpOnly"),
    ("__Host-wamn-csrf", "/", ""),
    ("__Secure-wamn-renewal", "/password", "; HttpOnly"),
];

fn set_cookies(response: &reqwest::Response) -> Vec<String> {
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|value| value.to_str().expect_redacted("ASCII cookie").to_owned())
        .collect()
}

/// Checks the exact reply of a cookie session and returns the session, CSRF and renewal values.
async fn cookie_body(response: reqwest::Response, started: i64) -> (Value, [String; 3]) {
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let lines = set_cookies(&response);
    let bytes = response.bytes().await.expect_redacted("response bytes");
    let returned = unix_seconds();
    let result: Value = serde_json::from_slice(&bytes).expect_redacted("response JSON");
    assert_fields(&result, &["expires_at", "login_expires_at"]);
    let deadlines = [
        result["expires_at"].as_i64().unwrap(),
        result["expires_at"].as_i64().unwrap(),
        result["login_expires_at"].as_i64().unwrap(),
    ];
    assert_eq!(lines.len(), 3);
    let mut values = [String::new(), String::new(), String::new()];
    for (index, line) in lines.iter().enumerate() {
        let (name, path, http_only) = COOKIES[index];
        let (pair, attributes) = line.split_once("; ").expect_redacted("cookie attributes");
        let value = pair
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('='))
            .expect_redacted("cookie name");
        let age: i64 = attributes
            .strip_prefix(&format!("Path={path}; Max-Age="))
            .and_then(|rest| rest.split(';').next())
            .and_then(|age| age.parse().ok())
            .expect_redacted("cookie Max-Age");
        assert_eq!(
            attributes,
            format!("Path={path}; Max-Age={age}{http_only}; Secure; SameSite=Strict")
        );
        assert!(age > 0 && deadlines[index] - returned <= age && age <= deadlines[index] - started);
        // No token or CSRF value ever appears in a cookie-mode body.
        assert!(!value.is_empty() && !String::from_utf8_lossy(&bytes).contains(value));
        value.clone_into(&mut values[index]);
    }
    assert!(values[2].starts_with("wamn_renew_"));
    (result, values)
}

fn assert_cleared(response: &reqwest::Response) {
    assert_eq!(response.status(), 204);
    let expected: Vec<String> = COOKIES
        .iter()
        .map(|(name, path, http_only)| {
            format!("{name}=; Path={path}; Max-Age=0{http_only}; Secure; SameSite=Strict")
        })
        .collect();
    assert_eq!(set_cookies(response), expected);
}

/// The session cookie verifies and its csrf claim is the SHA-256 hex of the CSRF cookie.
async fn cookie_claims(fixture: &Fixture, values: &[String; 3]) -> SessionClaims {
    let verified = claims(fixture, &json!({"access_token": values[0]})).await;
    #[expect(
        clippy::manual_assert_eq,
        reason = "never print CSRF values on failure"
    )]
    {
        assert!(verified.csrf == Some(hex::encode(Sha256::digest(values[1].as_bytes()))));
    }
    verified
}

fn by_cookie(
    https: &Https,
    fixture: &Fixture,
    path: &str,
    cookie: Option<String>,
) -> reqwest::RequestBuilder {
    let request = post(
        https,
        path,
        &json!({"aud":fixture.targets[0].audience(),"carrier":"cookie"}),
    );
    match cookie {
        Some(cookie) => request.header("cookie", cookie),
        None => request,
    }
}

fn renewal(values: &[String; 3]) -> String {
    format!("theme=dark; __Secure-wamn-renewal={}", values[2])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cookie_carrier_sets_rotates_and_clears_three_cookies() {
    let mut postgres = wamn_test_postgres::start(&[]).expect_redacted("owned PostgreSQL");
    let db = postgres
        .create_database("wamn_system")
        .expect_redacted("owned system database");
    let fixture = setup(db.url()).await;
    enroll(&fixture, EMAIL, PASSWORD).await;
    let https = server(&fixture).await;
    let cookie_login = || {
        post(
            &https,
            "/password/session",
            &json!({"email":EMAIL,"password":PASSWORD,"aud":fixture.targets[0].audience(),"carrier":"cookie"}),
        )
        .send()
    };

    // The bearer carrier, default or explicit, keeps its frozen reply and sets no cookie.
    discovery_window(&fixture).await;
    let explicit = post(
        &https,
        "/password/session",
        &json!({"email":EMAIL,"password":PASSWORD,"aud":fixture.targets[0].audience(),"carrier":"bearer"}),
    )
    .send()
    .await
    .unwrap();
    assert!(set_cookies(&explicit).is_empty());
    let bearer = body(explicit).await;
    assert!(claims(&fixture, &bearer).await.csrf.is_none());
    let default = post(
        &https,
        "/password/session",
        &json!({"email":EMAIL,"password":PASSWORD,"aud":fixture.targets[0].audience()}),
    )
    .send()
    .await
    .unwrap();
    assert!(set_cookies(&default).is_empty());
    body(default).await;
    let renewed = request(&https, &fixture, "/password/renew", &bearer)
        .send()
        .await
        .unwrap();
    assert!(set_cookies(&renewed).is_empty());
    body(renewed).await;

    let started = unix_seconds();
    let (first_body, first) = cookie_body(cookie_login().await.unwrap(), started).await;
    let first_claims = cookie_claims(&fixture, &first).await;
    assert_eq!(first_body["expires_at"], first_claims.exp);

    // The carrier decides where the renewal token may travel.
    for (path, body) in [
        (
            "/password/renew",
            json!({"aud":fixture.targets[0].audience(),"carrier":"cookie","renewal_token":first[2]}),
        ),
        (
            "/password/renew",
            json!({"aud":fixture.targets[0].audience()}),
        ),
        (
            "/password/logout-all",
            json!({"aud":fixture.targets[0].audience(),"carrier":"cookie"}),
        ),
    ] {
        assert_failure(
            post(&https, path, &body)
                .header("cookie", renewal(&first))
                .send()
                .await
                .unwrap(),
            400,
            "{\"error\":\"password request refused\"}",
        )
        .await;
    }
    // A missing or duplicated renewal cookie refuses and consumes nothing.
    let duplicated = Some(format!(
        "{}; __Secure-wamn-renewal={}",
        renewal(&first),
        first[2]
    ));
    for cookie in [None, duplicated] {
        assert_failure(
            by_cookie(&https, &fixture, "/password/renew", cookie)
                .send()
                .await
                .unwrap(),
            401,
            "{\"error\":\"unauthorized\"}",
        )
        .await;
    }

    let started = unix_seconds();
    let (second_body, second) = cookie_body(
        by_cookie(&https, &fixture, "/password/renew", Some(renewal(&first)))
            .send()
            .await
            .unwrap(),
        started,
    )
    .await;
    assert_eq!(
        first_body["login_expires_at"],
        second_body["login_expires_at"]
    );
    for index in 0..3 {
        assert!(first[index] != second[index], "every cookie rotates");
    }
    let second_claims = cookie_claims(&fixture, &second).await;
    assert_eq!(second_claims.authority, first_claims.authority);

    // A replayed renewal cookie refuses and revokes its family.
    assert_failure(
        by_cookie(&https, &fixture, "/password/renew", Some(renewal(&first)))
            .send()
            .await
            .unwrap(),
        401,
        "{\"error\":\"unauthorized\"}",
    )
    .await;
    assert_failure(
        by_cookie(&https, &fixture, "/password/renew", Some(renewal(&second)))
            .send()
            .await
            .unwrap(),
        401,
        "{\"error\":\"unauthorized\"}",
    )
    .await;
    assert!(!active(&fixture, &json!({"access_token": second[0]})).await);

    // Logout by cookie revokes the login and clears all three cookies.
    discovery_window(&fixture).await;
    let (_, third) = cookie_body(cookie_login().await.unwrap(), unix_seconds()).await;
    assert!(active(&fixture, &json!({"access_token": third[0]})).await);
    assert_cleared(
        &by_cookie(&https, &fixture, "/password/logout", Some(renewal(&third)))
            .send()
            .await
            .unwrap(),
    );
    assert!(!active(&fixture, &json!({"access_token": third[0]})).await);
    // A replayed or missing renewal cookie answers as bearer logout does and still clears.
    for cookie in [Some(renewal(&third)), None] {
        assert_cleared(
            &by_cookie(&https, &fixture, "/password/logout", cookie)
                .send()
                .await
                .unwrap(),
        );
    }
    assert_failure(
        by_cookie(&https, &fixture, "/password/renew", Some(renewal(&third)))
            .send()
            .await
            .unwrap(),
        401,
        "{\"error\":\"unauthorized\"}",
    )
    .await;
    drop(https);
    cleanup(fixture).await;
}
