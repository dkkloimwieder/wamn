//! HTTPS lifecycle checks share the exchange suite's owned database and TLS fixtures.

use super::*;
use wamn_platform_identity::password::{
    Password, enroll_password, issue_invitation, password_work,
};

const EMAIL: &str = "renewal-alice@example.invalid";

async fn enroll(fixture: &Fixture, email: &str, password: &str) -> Principal {
    let person = create_human(&fixture.system.client, email, email, "Renewal person")
        .await
        .expect_redacted("human");
    seed_user(&fixture.environments[0].client, &person, TENANT, "receiver").await;
    grant(&fixture.system.client, &person, "acme", "dev").await;
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
            org: "acme",
            audience: fixture.targets[0].audience(),
        },
        unix_seconds(),
    )
    .expect_redacted("signed session")
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
    // Revocation stops renewal, while a previously issued access token still verifies.
    claims(&fixture, &second).await;

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
        "acme",
        "receiving",
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
    grant(&fixture.system.client, &person, "acme", "dev").await;
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
    fixture.system.client.batch_execute("UPDATE registry.project_envs SET instance_suffix='replaced' WHERE org='acme' AND env='dev'").await.unwrap();
    assert_eq!(
        request(&https, &fixture, "/password/renew", &active)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    fixture.system.client.batch_execute("UPDATE registry.project_envs SET instance_suffix='s3ss10n2' WHERE org='acme' AND env='dev'").await.unwrap();
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
    claims(&fixture, &active).await;

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
    disable_principal(&fixture.system.client, person.id())
        .await
        .unwrap();
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
    tokio::time::timeout(Duration::from_secs(2),async{
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

    let other = enroll(
        &fixture,
        "new-password@example.invalid",
        "a different valid replacement password",
    )
    .await;
    let new_hash:String=fixture.system.client.query_one("SELECT password_hash FROM identity.password_credentials WHERE principal_id=$1::text::uuid",&[&other.id().as_str()]).await.unwrap().get(0);
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
    let pending = post(
        &https,
        "/password/session",
        &json!({"email":EMAIL,"password":PASSWORD,"aud":fixture.targets[0].audience()}),
    );
    let old_login = tokio::spawn(async move { pending.send().await });
    wait_for_blocked_issuer(&fixture.system.client, &fixture.issuer_role).await;
    // Simulate the reset transaction; the reset HTTP endpoint is a later issue.
    fixture.system.client.execute("UPDATE identity.password_credentials SET password_hash=$2 WHERE principal_id=$1::text::uuid",&[&person.id().as_str(),&new_hash]).await.unwrap();
    fixture.system.client.execute("UPDATE identity.password_logins SET revoked_at=clock_timestamp() WHERE principal_id=$1::text::uuid",&[&person.id().as_str()]).await.unwrap();
    fixture.system.client.batch_execute("COMMIT").await.unwrap();
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
