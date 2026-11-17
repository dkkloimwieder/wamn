//! Membership ordering and application authority on private PostgreSQL.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};
use wamn_provision::test_database;
use wamn_identity::org::{
    activate_org_membership, deactivate_org_membership, grant_project_admin, lock_membership,
};
use wamn_identity::create_or_reuse_user;

async fn connect(url: &str) -> anyhow::Result<Client> {
    let mut client = wamn_postgres::connect(url).await?;
    sqlx::query("INSERT INTO platform.login_principal(login,principal_id) VALUES(current_user,$1::text::uuid) ON CONFLICT(login) DO UPDATE SET principal_id=EXCLUDED.principal_id")
        .bind("770df186-ac15-579e-b46b-c297cae2011b").execute(&mut client).await?;

    Ok(client)
}

async fn blocked(observer: &mut Client, waiting: i32, holder: i32) -> anyhow::Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let found: bool = sqlx::query("SELECT $2=ANY(pg_blocking_pids($1))")
                .bind(&waiting)
                .bind(&holder)
                .fetch_one(&mut *observer)
                .await?
                .get(0);
            if found {
                return anyhow::Ok(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn inactive_membership_serializes_grants_and_refuses_sessions(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    test_database::system(database.as_str()).await;
    let mut first = connect(database.as_str()).await?;
    let mut observer = connect(database.as_str()).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&test_database::seed_org_project_env_sql(
            &"acme".parse().unwrap(),
            &"billing".parse().unwrap(),
            &"dev".parse().unwrap(),
        ))
            .to_string(),
    ))
    .execute(&mut first)
    .await
    .map(drop)?;
    let user = create_or_reuse_user(&mut first, "member@example.test", "Member")
        .await?
        .principal_id;
    activate_org_membership(&mut first, &user, &"acme".parse().unwrap()).await?;
    let holder: i32 = sqlx::query("SELECT pg_backend_pid()")
        .fetch_one(&mut first)
        .await?
        .get(0);
    let mut second = connect(database.as_str()).await?;
    let waiting: i32 = sqlx::query("SELECT pg_backend_pid()")
        .fetch_one(&mut second)
        .await?
        .get(0);
    let mut tx = first.begin().await?;
    lock_membership(&mut tx, &user, &"acme".parse().unwrap()).await?;
    let target = user;
    let removal = tokio::spawn(async move {
        let mut tx = second.begin().await?;
        deactivate_org_membership(&mut tx, &target, &"acme".parse().unwrap()).await?;
        tx.commit().await?;
        anyhow::Ok(second)
    });
    blocked(&mut observer, waiting, holder).await?;
    grant_project_admin(
        &mut tx,
        &user,
        &"acme".parse().unwrap(),
        &"billing".parse().unwrap(),
    )
    .await?;
    tx.commit().await?;
    let mut second = removal.await??;
    let remaining: i64 = sqlx::query(
        "SELECT count(*) FROM identity.project_roles WHERE principal_id=$1::text::uuid",
    )
    .bind(&user.to_string().as_str())
    .fetch_one(&mut observer)
    .await?
    .get(0);
    assert_eq!(remaining, 0);

    activate_org_membership(&mut first, &user, &"acme".parse().unwrap()).await?;
    let mut tx = first.begin().await?;
    deactivate_org_membership(&mut tx, &user, &"acme".parse().unwrap()).await?;
    let target = user;
    let grant = tokio::spawn(async move {
        let mut tx = second.begin().await?;
        let refusal = grant_project_admin(
            &mut tx,
            &target,
            &"acme".parse().unwrap(),
            &"billing".parse().unwrap(),
        )
        .await
        .expect_err("inactive membership refuses after the lock wait");
        assert_eq!(
            refusal.refusal(),
            Some(wamn_identity::IdentityRefusal::UserNotActive)
        );
        tx.rollback().await?;
        anyhow::Ok(second)
    });
    blocked(&mut observer, waiting, holder).await?;
    tx.commit().await?;
    let _second = grant.await??;

    // Retain inconsistent access rows to test membership independently of roles.
    sqlx::query("INSERT INTO identity.project_roles(principal_id,org,project,role) VALUES($1::text::uuid,'acme','billing','project-admin')").bind(&user.to_string().as_str()).execute(&mut first).await.map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO identity.project_env_memberships(principal_id,org,project,env) VALUES($1::text::uuid,'acme','billing','dev')").bind(&user.to_string().as_str()).execute(&mut first).await.map(|result| result.rows_affected())?;
    let issuer = "https://identity.example.test";
    let audience = "urn:wamn:control:acme";
    let mut tx = first.begin().await?;
    let login =
        wamn_identity::password_login::create_login(&mut tx, &user, issuer, audience)
            .await?
            .expect("active global user");
    tx.commit().await?;
    let claims: wamn_identity_protocol::token::SessionClaims = serde_json::from_value(
        serde_json::json!({
            "iss": issuer, "sub": user.to_string().as_str(), "org": "acme", "aud": audience,
            "roles": ["admin"], "iat": 1, "exp": 901, "jti": "membership-test", "authority": {"login": login.login.id}
        }),
    )?;
    assert!(
        !wamn_identity::control::control_session_is_active(&mut first, &claims).await?
    );
    assert!(
        !wamn_identity::session_token::session_is_active(
            &mut first,
            &claims,
            &"billing".parse().unwrap(),
            &"dev".parse().unwrap()
        )
        .await?
    );
    assert!(
        !wamn_identity::has_project_env_membership(
            &mut first,
            &user,
            &"acme".parse().unwrap(),
            &"billing".parse().unwrap(),
            &"dev".parse().unwrap()
        )
        .await?
    );
    assert!(
        !wamn_identity::control::is_project_admin(
            &mut first,
            &user,
            &"acme".parse().unwrap(),
            &"billing".parse().unwrap(),
            None
        )
        .await?
    );
    Ok(())
}
