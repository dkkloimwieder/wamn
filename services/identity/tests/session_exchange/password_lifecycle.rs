//! HTTPS lifecycle tests share the exchange suite's owned database and TLS fixtures.

use super::*;
use std::collections::BTreeMap;
use wamn_oci::package_artifact::http_client::HttpRequest;
use wamn_platform_identity::password::{
    Password, enroll_password, issue_invitation, password_work,
};

const EMAIL: &str = "renewal-alice@example.invalid";

async fn fixture_transaction(
    connection: &mut sqlx::PgConnection,
) -> sqlx::Transaction<'_, sqlx::Postgres> {
    wamn_test_postgres::transaction(
        connection,
        &PlatformComponent::Provisioning.principal_id(),
        "admin:password-lifecycle",
    )
    .await
    .expect_redacted("bind fixture principal")
}

async fn enroll(fixture: &mut Fixture, email: &str, password: &str) -> Principal {
    let user = create_user(&mut fixture.system.client, email, email, "Renewal user")
        .await
        .expect_redacted("user");
    seed_user(&mut fixture.environments[0].client, &user, "receiver").await;
    grant(
        &mut fixture.system.client,
        &user,
        &"demo".parse().unwrap(),
        &"dev".parse().unwrap(),
    )
    .await;
    let actor = PlatformComponent::Provisioning.principal_id();
    let mut issuer = connect(&fixture.issuer_url).await;
    let invitation = issue_invitation(&mut issuer.client, &actor, user.id())
        .await
        .expect_redacted("invitation");
    enroll_password(
        &mut issuer.client,
        &password_work(),
        user.id(),
        invitation.secret(),
        Password::new(password.to_owned()).unwrap(),
    )
    .await
    .expect_redacted("enrollment");
    user
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
fn post(https: &Https, path: &str, body: &Value) -> HttpRequest {
    https
        .client
        .post(format!("{}{path}", https.endpoint))
        .json(body)
}
fn request(https: &Https, fixture: &Fixture, path: &str, token: &Value) -> HttpRequest {
    post(
        https,
        path,
        &json!({"aud":fixture.targets[0].audience(),"renewal_token":token["renewal_token"]}),
    )
}
async fn body(response: HttpResponse) -> Value {
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
            "sub",
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
    discovery_window(&fixture.observations).await;
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
    let jwks = wamn_platform_identity::session_keys::session_jwks(
        &mut *fixture.observations.acquire().await.unwrap(),
        ISSUER,
    )
    .await
    .expect_redacted("JWKS");
    let token = body["access_token"].as_str().expect_redacted("token");
    let kid = jsonwebtoken::decode_header(token)
        .expect_redacted("kid")
        .kid
        .expect("key ID");
    let key = jwks
        .keys
        .iter()
        .find(|key| key.kid == kid)
        .expect_redacted("public key");
    verify(
        token,
        key,
        Audience {
            issuer: ISSUER,
            org: "demo",
            audience: fixture.targets[0].audience(),
        },
        unix_seconds(),
    )
    .await
    .expect_redacted("signed session")
}

async fn active(fixture: &Fixture, body: &Value) -> bool {
    wamn_platform_identity::session_token::session_is_active(
        &mut *fixture.observations.acquire().await.unwrap(),
        &claims(fixture, body).await,
        &"widgets".parse().unwrap(),
        &"dev".parse().unwrap(),
    )
    .await
    .expect_redacted("current session authority")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires: postgres"]
async fn renewal_rotation_authority_expiry_and_logout_use_real_https() {
    let mut postgres = wamn_test_postgres::start(&[]).expect_redacted("owned PostgreSQL");
    let db = postgres
        .create_database("wamn_system")
        .await
        .expect_redacted("owned system database");
    let mut fixture = setup(db.url()).await;
    let user = enroll(&mut fixture, EMAIL, PASSWORD).await;
    let https = server(&fixture).await;
    let first = login(&https, &fixture).await;
    let verified = claims(&fixture, &first).await;
    assert_eq!(first["sub"], serde_json::to_value(verified.sub).unwrap());
    assert_eq!(verified.sub, *user.id());
    assert_eq!(
        verified.roles,
        vec![
            "receiver"
                .parse::<wamn_identity_protocol::RoleName>()
                .unwrap()
        ]
    );
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
    assert_eq!(
        second["sub"],
        serde_json::to_value(claims(&fixture, &second).await.sub).unwrap()
    );
    assert_failure(
        request(&https, &fixture, "/password/renew", &first)
            .send()
            .await
            .unwrap(),
        401,
        "unauthorized",
    )
    .await;
    assert_failure(
        request(&https, &fixture, "/password/renew", &second)
            .send()
            .await
            .unwrap(),
        401,
        "unauthorized",
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
    assert_failure(wrong, 401, "unauthorized").await;
    revoke_project_env_membership(
        &mut fixture.system.client,
        user.id(),
        &"demo".parse().unwrap(),
        &"widgets".parse().unwrap(),
        &"dev".parse().unwrap(),
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
    grant(
        &mut fixture.system.client,
        &user,
        &"demo".parse().unwrap(),
        &"dev".parse().unwrap(),
    )
    .await;
    let mut mutation = fixture_transaction(&mut fixture.environments[0].client).await;
    sqlx::query("UPDATE platform.\"user\" SET status='disabled' WHERE id=$1::text::uuid")
        .bind(&user.id().to_string().as_str())
        .execute(&mut *mutation)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    mutation
        .commit()
        .await
        .expect_redacted("commit fixture mutation");
    assert_eq!(
        request(&https, &fixture, "/password/renew", &active)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let mut mutation = fixture_transaction(&mut fixture.environments[0].client).await;
    sqlx::query("UPDATE platform.\"user\" SET status='active' WHERE id=$1::text::uuid")
        .bind(&user.id().to_string().as_str())
        .execute(&mut *mutation)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    mutation
        .commit()
        .await
        .expect_redacted("commit fixture mutation");
    let mut mutation = fixture_transaction(&mut fixture.environments[0].client).await;
    sqlx::query("DELETE FROM platform.user_role WHERE user_id=$1::text::uuid")
        .bind(&user.id().to_string().as_str())
        .execute(&mut *mutation)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    mutation
        .commit()
        .await
        .expect_redacted("commit fixture mutation");
    assert_eq!(
        request(&https, &fixture, "/password/renew", &active)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let mut mutation = fixture_transaction(&mut fixture.environments[0].client).await;
    sqlx::query(
        "INSERT INTO platform.user_role (user_id,role_name) VALUES ($1::text::uuid,'receiver')",
    )
    .bind(&user.id().to_string().as_str())
    .execute(&mut *mutation)
    .await
    .map(|result| result.rows_affected())
    .unwrap();
    mutation
        .commit()
        .await
        .expect_redacted("commit fixture mutation");
    let mut mutation = fixture_transaction(&mut fixture.system.client).await;
    sqlx::raw_sql("UPDATE registry.environment_projects SET instance_suffix='replaced' WHERE org='demo' AND env='dev'").execute(&mut *mutation).await.map(drop).unwrap();
    mutation
        .commit()
        .await
        .expect_redacted("commit fixture mutation");
    assert_eq!(
        request(&https, &fixture, "/password/renew", &active)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let mut mutation = fixture_transaction(&mut fixture.system.client).await;
    sqlx::raw_sql("UPDATE registry.environment_projects SET instance_suffix='s3ss10n2' WHERE org='demo' AND env='dev'").execute(&mut *mutation).await.map(drop).unwrap();
    mutation
        .commit()
        .await
        .expect_redacted("commit fixture mutation");
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
    let mut mutation = fixture_transaction(&mut fixture.system.client).await;
    sqlx::query("WITH deadline AS (SELECT date_trunc('second',clock_timestamp())+interval '60 seconds' AS at) UPDATE identity.password_logins SET authenticated_at=deadline.at-interval '8 hours',expires_at=deadline.at,renewal_expires_at=deadline.at FROM deadline WHERE principal_id=$1::text::uuid AND revoked_at IS NULL").bind(&user.id().to_string().as_str()).execute(&mut *mutation).await.map(|result| result.rows_affected()).unwrap();
    mutation
        .commit()
        .await
        .expect_redacted("commit fixture mutation");
    let bounded = body(
        request(&https, &fixture, "/password/renew", &near_expiry)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(bounded["expires_at"], bounded["login_expires_at"]);
    assert!(claims(&fixture, &bounded).await.exp <= unix_seconds() + 60);
    let mut mutation = fixture_transaction(&mut fixture.system.client).await;
    sqlx::query("UPDATE identity.password_logins SET authenticated_at=authenticated_at-interval '9 hours',expires_at=expires_at-interval '9 hours',renewal_expires_at=renewal_expires_at-interval '9 hours' WHERE principal_id=$1::text::uuid").bind(&user.id().to_string().as_str()).execute(&mut *mutation).await.map(|result| result.rows_affected()).unwrap();
    mutation
        .commit()
        .await
        .expect_redacted("commit fixture mutation");
    assert_eq!(
        request(&https, &fixture, "/password/renew", &bounded)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        sqlx::query("SELECT count(*) FROM identity.password_logins")
            .fetch_one(&mut fixture.system.client)
            .await
            .unwrap()
            .get::<i64, _>(0),
        0
    );
    let disabled = login(&https, &fixture).await;
    assert!(self::active(&fixture, &disabled).await);
    disable_principal(&mut fixture.system.client, user.id())
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

/// A control session (docs/architecture/execution.md#control-sessions) is
/// offered, issued and renewed only while the user holds `project-admin` or
/// `org-admin` in the org, carries no application role, and never comes from a
/// PAT.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires: postgres"]
async fn a_control_session_follows_the_admin_roles_of_its_org() {
    const CONTROL: &str = "urn:wamn:control:demo";
    let mut postgres = wamn_test_postgres::start(&[]).expect_redacted("owned PostgreSQL");
    let db = postgres
        .create_database("wamn_system")
        .await
        .expect_redacted("owned system database");
    let mut fixture = setup(db.url()).await;
    let user = enroll(&mut fixture, EMAIL, PASSWORD).await;
    let https = server(&fixture).await;
    let audiences = |listed: &Value| -> Vec<Value> {
        listed["environments"]
            .as_array()
            .expect_redacted("environments")
            .clone()
    };
    let discover = async |https: &Https| {
        discovery_window(&fixture.observations).await;
        let response = post(
            https,
            "/password/environments",
            &json!({"email": EMAIL, "password": PASSWORD}),
        )
        .send()
        .await
        .expect_redacted("discovery");
        assert_eq!(response.status(), 200);
        response
            .json::<Value>()
            .await
            .expect_redacted("discovery JSON")
    };
    let control_login = async |https: &Https| {
        discovery_window(&fixture.observations).await;
        post(
            https,
            "/password/session",
            &json!({"email": EMAIL, "password": PASSWORD, "aud": CONTROL}),
        )
        .send()
        .await
        .expect_redacted("control login")
    };

    assert!(
        !audiences(&discover(&https).await)
            .iter()
            .any(|entry| entry["aud"] == CONTROL),
        "no control role, no control audience"
    );
    assert_failure(control_login(&https).await, 401, "unauthorized").await;

    wamn_platform_identity::assign_project_role(
        &mut fixture.system.client,
        user.id(),
        &"demo".parse().unwrap(),
        &"widgets".parse().unwrap(),
        "project-admin",
    )
    .await
    .expect_redacted("project-admin");
    assert!(
        audiences(&discover(&https).await).contains(&json!({"aud": CONTROL, "org": "demo"})),
        "a project-admin is offered the control audience of the org"
    );
    // An application shell names its org and project, and is offered the
    // control audience of its org beside the environments (wamn-a40n.11).
    let targeted = async |org: &wamn_registry::OrgId| {
        discovery_window(&fixture.observations).await;
        let response = post(
            &https,
            "/password/environments",
            &json!({"email": EMAIL, "password": PASSWORD, "org": org, "project": "widgets"}),
        )
        .send()
        .await
        .expect_redacted("targeted discovery");
        assert_eq!(response.status(), 200);
        response
            .json::<Value>()
            .await
            .expect_redacted("targeted discovery JSON")
    };
    assert!(
        audiences(&targeted(&"demo".parse().unwrap()).await)
            .contains(&json!({"aud": CONTROL, "org": "demo"})),
        "a request that names a project is offered the control audience of its org"
    );
    assert!(
        !audiences(&targeted(&"other".parse().unwrap()).await)
            .iter()
            .any(|entry| entry["aud"] == CONTROL),
        "a request for another org is not offered this org's control audience"
    );
    let session = body(control_login(&https).await).await;
    let jwks = wamn_platform_identity::session_keys::session_jwks(
        &mut *fixture.observations.acquire().await.unwrap(),
        ISSUER,
    )
    .await
    .expect_redacted("JWKS");
    let token = session["access_token"].as_str().expect_redacted("token");
    let kid = jsonwebtoken::decode_header(token)
        .expect_redacted("kid")
        .kid
        .expect("key ID");
    let key = jwks
        .keys
        .iter()
        .find(|key| key.kid == kid)
        .expect_redacted("public key");
    let claims = verify(
        token,
        key,
        Audience {
            issuer: ISSUER,
            org: "demo",
            audience: CONTROL,
        },
        unix_seconds(),
    )
    .await
    .expect_redacted("signed control session");
    assert_eq!(claims.sub, *user.id());
    assert_eq!(session["sub"], serde_json::to_value(claims.sub).unwrap());
    assert!(claims.roles.is_empty(), "a control session has no roles");
    assert!(
        wamn_platform_identity::control::control_session_is_active(
            &mut fixture.system.client,
            &claims
        )
        .await
        .expect_redacted("control authority")
    );

    let renewed = body(
        post(
            &https,
            "/password/renew",
            &json!({"aud": CONTROL, "renewal_token": session["renewal_token"]}),
        )
        .send()
        .await
        .expect_redacted("control renewal"),
    )
    .await;
    sqlx::query("DELETE FROM identity.project_roles WHERE principal_id = $1::text::uuid")
        .bind(&user.id().to_string().as_str())
        .execute(&mut fixture.system.client)
        .await
        .map(|result| result.rows_affected())
        .expect_redacted("revoke project-admin");
    assert!(
        !wamn_platform_identity::control::control_session_is_active(
            &mut fixture.system.client,
            &claims
        )
        .await
        .expect_redacted("control authority"),
        "a revoked role ends the control session on the next request"
    );
    assert_failure(
        post(
            &https,
            "/password/renew",
            &json!({"aud": CONTROL, "renewal_token": renewed["renewal_token"]}),
        )
        .send()
        .await
        .expect_redacted("refused renewal"),
        401,
        "unauthorized",
    )
    .await;

    // `org-admin` alone, with no project role, holds the control audience.
    sqlx::query(
        "INSERT INTO identity.org_roles (principal_id, org, role) \
             VALUES ($1::text::uuid, 'demo', 'org-admin')",
    )
    .bind(&user.id().to_string().as_str())
    .execute(&mut fixture.system.client)
    .await
    .map(|result| result.rows_affected())
    .expect_redacted("org-admin");
    assert!(
        audiences(&discover(&https).await).contains(&json!({"aud": CONTROL, "org": "demo"})),
        "an org-admin is offered the control audience of the org"
    );
    let session = body(control_login(&https).await).await;
    let token = session["access_token"].as_str().expect_redacted("token");
    let claims = verify(
        token,
        key,
        Audience {
            issuer: ISSUER,
            org: "demo",
            audience: CONTROL,
        },
        unix_seconds(),
    )
    .await
    .expect_redacted("signed org-admin control session");
    assert!(
        wamn_platform_identity::control::control_session_is_active(
            &mut fixture.system.client,
            &claims
        )
        .await
        .expect_redacted("control authority")
    );
    sqlx::query("DELETE FROM identity.org_roles WHERE principal_id = $1::text::uuid")
        .bind(&user.id().to_string().as_str())
        .execute(&mut fixture.system.client)
        .await
        .map(|result| result.rows_affected())
        .expect_redacted("revoke org-admin");
    assert!(
        !wamn_platform_identity::control::control_session_is_active(
            &mut fixture.system.client,
            &claims
        )
        .await
        .expect_redacted("control authority"),
        "a revoked org-admin ends the control session on the next request"
    );

    // An org member with no grant, as an invitation with no access leaves
    // it, signs in and is offered no environment.
    sqlx::query("DELETE FROM identity.project_env_memberships WHERE principal_id = $1::text::uuid")
        .bind(&user.id().to_string().as_str())
        .execute(&mut fixture.system.client)
        .await
        .map(|result| result.rows_affected())
        .expect_redacted("remove the memberships");
    {
        let observed = &(discover(&https).await);
        assert!(
            observed["environments"]
                .as_array()
                .expect("array")
                .is_empty()
        );
        assert_eq!(observed["next_cursor"], serde_json::json!(null));
        assert_eq!(observed["total"], serde_json::json!(0));
    };
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires: postgres"]
async fn login_reset_and_renewal_logout_races_keep_transaction_order() {
    let mut postgres = wamn_test_postgres::start(&[]).expect_redacted("owned PostgreSQL");
    let db = postgres
        .create_database("wamn_system")
        .await
        .expect_redacted("owned system database");
    let mut fixture = setup(db.url()).await;
    let user = enroll(&mut fixture, EMAIL, PASSWORD).await;
    let https = server(&fixture).await;
    let first = login(&https, &fixture).await;
    sqlx::raw_sql("BEGIN; SELECT issuer FROM identity.session_signing_state FOR UPDATE")
        .execute(&mut fixture.system.client)
        .await
        .map(drop)
        .unwrap();
    let pending = request(&https, &fixture, "/password/renew", &first);
    let renewal = tokio::spawn(async move { pending.send().await });
    wait_for_blocked_issuer(&mut fixture.system.client, &fixture.issuer_role).await;
    let pending = request(&https, &fixture, "/password/logout-all", &first);
    let logout = tokio::spawn(async move { pending.send().await });
    tokio::time::timeout(HANG_GUARD,async{
        loop {
            sqlx::query("SELECT pg_stat_clear_snapshot()").fetch_one(&mut fixture.system.client).await.unwrap();
            let waiting:i64=sqlx::query("SELECT count(*) FROM pg_stat_activity WHERE usename=$1 AND cardinality(pg_blocking_pids(pid))>0").bind(&fixture.issuer_role).fetch_one(&mut fixture.system.client).await.unwrap().get(0);
            if waiting>=2 {break}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect_redacted("renewal and logout reach their locks");
    sqlx::raw_sql("COMMIT")
        .execute(&mut fixture.system.client)
        .await
        .map(drop)
        .unwrap();
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
    let actor = PlatformComponent::Provisioning.principal_id();
    let reset_secret = wamn_platform_identity::password::issue_reset(
        &mut reset_connection.client,
        &actor,
        user.id(),
        EMAIL,
    )
    .await
    .unwrap();
    discovery_window(&fixture.observations).await;
    sqlx::raw_sql("BEGIN")
        .execute(&mut fixture.system.client)
        .await
        .map(drop)
        .unwrap();
    sqlx::query("SELECT id FROM identity.principals WHERE id=$1::text::uuid FOR UPDATE")
        .bind(&user.id().to_string().as_str())
        .fetch_one(&mut fixture.system.client)
        .await
        .unwrap();
    let reset_principal = *user.id();
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
    wait_for_blocked_issuer(&mut fixture.system.client, &fixture.issuer_role).await;
    let pending = post(
        &https,
        "/password/session",
        &json!({"email":EMAIL,"password":PASSWORD,"aud":fixture.targets[0].audience()}),
    );
    let old_login = tokio::spawn(async move { pending.send().await });
    let pending_renewal = request(&https, &fixture, "/password/renew", &before_reset);
    let reset_racing_renewal = tokio::spawn(async move { pending_renewal.send().await });
    wait_for_blocked_issuer(&mut fixture.system.client, &fixture.issuer_role).await;
    sqlx::raw_sql("COMMIT")
        .execute(&mut fixture.system.client)
        .await
        .map(drop)
        .unwrap();
    reset.await.unwrap().unwrap();
    assert!(!active(&fixture, &before_reset).await);
    assert_eq!(reset_racing_renewal.await.unwrap().unwrap().status(), 401);
    assert_failure(old_login.await.unwrap().unwrap(), 401, "unauthorized").await;
    assert_eq!(sqlx::query("SELECT count(*) FROM identity.password_logins WHERE principal_id=$1::text::uuid AND revoked_at IS NULL").bind(&user.id().to_string().as_str()).fetch_one(&mut fixture.system.client).await.unwrap().get::<i64, _>(0),0);
    drop(https);
    cleanup(fixture).await;
}

const COOKIES: [(&str, &str, bool); 3] = [
    ("__Host-wamn-session", "/", true),
    ("__Host-wamn-csrf", "/", false),
    ("__Secure-wamn-renewal", "/password", true),
];

fn set_cookies(response: &HttpResponse) -> Vec<String> {
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|value| value.to_str().expect_redacted("ASCII cookie").to_owned())
        .collect()
}

struct Cookie<'a> {
    value: &'a str,
    attributes: BTreeMap<String, Option<&'a str>>,
}

impl Cookie<'_> {
    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes.get(name).copied().flatten()
    }
}

fn parse_cookies(lines: &[String]) -> BTreeMap<&str, Cookie<'_>> {
    lines
        .iter()
        .map(|line| {
            let mut parts = line.split(';').map(str::trim);
            let (name, value) = parts
                .next()
                .and_then(|pair| pair.split_once('='))
                .expect_redacted("cookie name and value");
            let (name, value) = (name.trim(), value.trim());
            let attributes = parts
                .map(|attribute| {
                    let (name, value) = attribute
                        .split_once('=')
                        .map_or((attribute, None), |(name, value)| {
                            (name.trim(), Some(value.trim()))
                        });
                    (name.to_ascii_lowercase(), value)
                })
                .collect();
            (name, Cookie { value, attributes })
        })
        .collect()
}

fn assert_cookie_attributes(cookie: &Cookie<'_>, path: &str, http_only: bool) -> i64 {
    assert!(cookie.attribute("path") == Some(path));
    assert!(cookie.attributes.contains_key("secure"));
    assert_eq!(cookie.attributes.contains_key("httponly"), http_only);
    assert!(
        cookie
            .attribute("samesite")
            .is_some_and(|value| value.eq_ignore_ascii_case("Strict"))
    );
    assert!(!cookie.attributes.contains_key("domain"));
    let age = cookie
        .attribute("max-age")
        .expect_redacted("cookie Max-Age");
    let digits = age.strip_prefix('-').unwrap_or(age);
    assert!(!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()));
    age.parse().expect_redacted("cookie Max-Age")
}

/// Check cookie fields and return the session, CSRF and renewal values.
async fn cookie_body(response: HttpResponse, started: i64) -> (Value, [String; 3]) {
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let lines = set_cookies(&response);
    let bytes = response.bytes().await.expect_redacted("response bytes");
    let returned = unix_seconds();
    let result: Value = serde_json::from_slice(&bytes).expect_redacted("response JSON");
    assert_fields(&result, &["expires_at", "login_expires_at", "sub"]);
    let deadlines = [
        result["expires_at"].as_i64().unwrap(),
        result["expires_at"].as_i64().unwrap(),
        result["login_expires_at"].as_i64().unwrap(),
    ];
    assert_eq!(lines.len(), COOKIES.len());
    let cookies = parse_cookies(&lines);
    let mut values = [String::new(), String::new(), String::new()];
    for (index, (name, path, http_only)) in COOKIES.into_iter().enumerate() {
        let cookie = cookies.get(name).expect_redacted("session cookie");
        let age = assert_cookie_attributes(cookie, path, http_only);
        assert!(age > 0 && deadlines[index] - returned <= age && age <= deadlines[index] - started);
        // No token or CSRF value ever appears in a cookie-mode body.
        assert!(
            !cookie.value.is_empty() && !String::from_utf8_lossy(&bytes).contains(cookie.value)
        );
        cookie.value.clone_into(&mut values[index]);
    }
    assert!(values[2].starts_with("wamn_renew_"));
    (result, values)
}

fn assert_cleared(response: &HttpResponse) {
    assert_eq!(response.status(), 204);
    let lines = set_cookies(response);
    assert_eq!(lines.len(), COOKIES.len());
    let cookies = parse_cookies(&lines);
    for (name, path, http_only) in COOKIES {
        let cookie = cookies.get(name).expect_redacted("cleared cookie");
        assert!(cookie.value.is_empty());
        assert_eq!(assert_cookie_attributes(cookie, path, http_only), 0);
    }
}

/// The session cookie verifies and its csrf JWT claim is the SHA-256 hex of the CSRF cookie.
async fn cookie_claims(fixture: &Fixture, values: &[String; 3]) -> SessionClaims {
    let verified = claims(fixture, &json!({"access_token": values[0]})).await;
    #[expect(
        clippy::manual_assert_eq,
        reason = "never print CSRF values on failure"
    )]
    {
        assert!(verified.csrf == Some(wamn_digest::bytes_sha256(values[1].as_bytes())));
    }
    verified
}

fn by_cookie(https: &Https, fixture: &Fixture, path: &str, cookie: Option<String>) -> HttpRequest {
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
#[ignore = "requires: postgres"]
async fn cookie_carrier_sets_rotates_and_clears_three_cookies() {
    let mut postgres = wamn_test_postgres::start(&[]).expect_redacted("owned PostgreSQL");
    let db = postgres
        .create_database("wamn_system")
        .await
        .expect_redacted("owned system database");
    let mut fixture = setup(db.url()).await;
    enroll(&mut fixture, EMAIL, PASSWORD).await;
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
    discovery_window(&fixture.observations).await;
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
    assert_eq!(
        first_body["sub"],
        serde_json::to_value(first_claims.sub).unwrap()
    );
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
            "password request refused",
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
            "unauthorized",
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
    assert_eq!(
        second_body["sub"],
        serde_json::to_value(second_claims.sub).unwrap()
    );
    assert_eq!(second_claims.authority, first_claims.authority);

    // A replayed renewal cookie refuses and revokes its family.
    assert_failure(
        by_cookie(&https, &fixture, "/password/renew", Some(renewal(&first)))
            .send()
            .await
            .unwrap(),
        401,
        "unauthorized",
    )
    .await;
    assert_failure(
        by_cookie(&https, &fixture, "/password/renew", Some(renewal(&second)))
            .send()
            .await
            .unwrap(),
        401,
        "unauthorized",
    )
    .await;
    assert!(!active(&fixture, &json!({"access_token": second[0]})).await);

    // Logout by cookie revokes the login and clears all three cookies.
    discovery_window(&fixture.observations).await;
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
        "unauthorized",
    )
    .await;
    drop(https);
    cleanup(fixture).await;
}
