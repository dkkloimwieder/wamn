//! Renewal storage, revocation races and the targeted issuer on real PostgreSQL.

use sha2::{Digest as _, Sha256};
use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};
use wamn_broker::auth_changes::AuthDrops;
use wamn_control_provision::{identity_role::grant_identity_issuer_surface_sql, test_database};
use wamn_identity_protocol::PrincipalId;
use wamn_platform_identity::password_login::{self as login, Renewal};
use wamn_platform_identity::{create_user, disable_principal};

const ISSUER: &str = "https://identity.example.invalid";
const AUDIENCE: &str = "demo:widgets:dev:a1b2c3d4";
const ACTOR: &str = "770df186-ac15-579e-b46b-c297cae2011b";

async fn connect(url: &str, role: &str) -> Client {
    let mut client = wamn_postgres::connect(url).await.unwrap();

    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET ROLE {role}")))
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    sqlx::query("SELECT set_config('app.user_id',$1,false)")
        .bind(&ACTOR)
        .execute(&mut client)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    client
}
async fn user(client: &mut Client) -> PrincipalId {
    *create_user(
        client,
        "alice@example.invalid",
        "alice@example.invalid",
        "Alice",
    )
    .await
    .unwrap()
    .id()
}
async fn create(client: &mut Client, user: &PrincipalId) -> Renewal {
    let mut tx = client.begin().await.unwrap();
    let issued = login::create_login(&mut tx, user, ISSUER, AUDIENCE)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    issued
}
async fn rotate(client: &mut Client, secret: &str) -> Option<Renewal> {
    let mut tx = client.begin().await.unwrap();
    let result = login::rotate_login(&mut tx, ISSUER, AUDIENCE, secret, &mut AuthDrops::default())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    result
}

#[sqlx::test(migrations = false)]
async fn targeted_rotation_preserves_deadline_and_replay_revokes_successor(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let db = wamn_test_postgres::url(&pool);
    test_database::system(db.as_str()).await;
    let mut owner = connect(db.as_str(), "wamn_system").await;
    let user = user(&mut owner).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(grant_identity_issuer_surface_sql()))
        .execute(&mut wamn_postgres::connect(db.as_str()).await.unwrap())
        .await
        .unwrap();
    let mut issuer = connect(db.as_str(), "wamn_identity_issuer").await;
    let first = create(&mut issuer, &user).await;
    assert_eq!(first.login.expires_at - first.login.authenticated_at, 28800);
    assert!(!format!("{first:?}").contains(first.secret()));
    let stored: String =
        sqlx::query("SELECT row_to_json(c)::text FROM identity.renewal_credentials c")
            .fetch_one(&mut owner)
            .await
            .unwrap()
            .get(0);
    assert!(!stored.contains(first.secret()));
    let hash: Vec<u8> = sqlx::query("SELECT token_hash FROM identity.renewal_credentials")
        .fetch_one(&mut owner)
        .await
        .unwrap()
        .get(0);
    assert_eq!(hash, Sha256::digest(first.secret()).to_vec());
    let mut tx = issuer.begin().await.unwrap();
    assert!(
        login::rotate_login(
            &mut tx,
            "https://other.invalid",
            AUDIENCE,
            first.secret(),
            &mut AuthDrops::default()
        )
        .await
        .unwrap()
        .is_none()
    );
    assert!(
        login::rotate_login(
            &mut tx,
            ISSUER,
            "other-instance",
            first.secret(),
            &mut AuthDrops::default()
        )
        .await
        .unwrap()
        .is_none()
    );
    tx.commit().await.unwrap();
    let mut tx = issuer.begin().await.unwrap();
    let rolled_back = login::rotate_login(
        &mut tx,
        ISSUER,
        AUDIENCE,
        first.secret(),
        &mut AuthDrops::default(),
    )
    .await
    .unwrap()
    .unwrap();
    tx.rollback().await.unwrap();
    assert!(rotate(&mut issuer, rolled_back.secret()).await.is_none());
    let second = rotate(&mut issuer, first.secret()).await.unwrap();
    assert_eq!(second.login.id, first.login.id);
    assert_eq!(second.login.authenticated_at, first.login.authenticated_at);
    assert_eq!(second.login.expires_at, first.login.expires_at);
    assert_ne!(second.secret(), first.secret());
    assert!(rotate(&mut issuer, first.secret()).await.is_none());
    assert!(rotate(&mut issuer, second.secret()).await.is_none());
    assert_eq!(
        sqlx::query("SELECT count(*) FROM identity.renewal_credentials")
            .fetch_one(&mut owner)
            .await
            .unwrap()
            .get::<i64, _>(0),
        2
    );
    // The issuer can rotate and revoke, but cannot rewrite account binding or deadlines.
    let error = sqlx::raw_sql(
        "UPDATE identity.password_logins SET expires_at=expires_at+interval '1 hour'",
    )
    .execute(&mut issuer)
    .await
    .map(drop)
    .unwrap_err();
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("42501")
    );
    let error = sqlx::raw_sql("DELETE FROM identity.renewal_credentials")
        .execute(&mut issuer)
        .await
        .map(drop)
        .unwrap_err();
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("42501")
    );
    let mut tx = issuer.begin().await.unwrap();
    assert_eq!(login::prune_expired(&mut tx, ISSUER).await.unwrap(), 0);
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = false)]
async fn inactivity_absolute_expiry_and_bounded_cleanup(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let db = wamn_test_postgres::url(&pool);
    test_database::system(db.as_str()).await;
    let mut owner = connect(db.as_str(), "wamn_system").await;
    let user = user(&mut owner).await;
    let idle = create(&mut owner, &user).await;
    sqlx::query("UPDATE identity.password_logins SET authenticated_at=authenticated_at-interval '1 hour',expires_at=expires_at-interval '1 hour',renewal_expires_at=clock_timestamp()-interval '1 second' WHERE id=$1::text::uuid").bind(&idle.login.id).execute(&mut owner).await.map(|result| result.rows_affected()).unwrap();
    assert!(rotate(&mut owner, idle.secret()).await.is_none());
    let final_credential = create(&mut owner, &user).await;
    sqlx::query("UPDATE identity.password_logins SET authenticated_at=authenticated_at-interval '7 hours 55 minutes',expires_at=expires_at-interval '7 hours 55 minutes',renewal_expires_at=expires_at-interval '7 hours 55 minutes' WHERE id=$1::text::uuid").bind(&final_credential.login.id).execute(&mut owner).await.map(|result| result.rows_affected()).unwrap();
    let last = rotate(&mut owner, final_credential.secret()).await.unwrap();
    assert!(sqlx::query("SELECT renewal_expires_at=expires_at FROM identity.password_logins WHERE id=$1::text::uuid").bind(&last.login.id).fetch_one(&mut owner).await.unwrap().get::<bool, _>(0));
    for _ in 0..99 {
        create(&mut owner, &user).await;
    }
    sqlx::raw_sql("UPDATE identity.password_logins SET authenticated_at=authenticated_at-interval '9 hours',expires_at=expires_at-interval '9 hours',renewal_expires_at=renewal_expires_at-interval '9 hours'").execute(&mut owner).await.map(drop).unwrap();
    assert!(rotate(&mut owner, last.secret()).await.is_none());
    sqlx::raw_sql(sqlx::AssertSqlSafe(grant_identity_issuer_surface_sql()))
        .execute(&mut wamn_postgres::connect(db.as_str()).await.unwrap())
        .await
        .unwrap();
    let mut issuer = connect(db.as_str(), "wamn_identity_issuer").await;
    let mut tx = issuer.begin().await.unwrap();
    assert_eq!(
        login::prune_expired(&mut tx, "https://other.invalid")
            .await
            .unwrap(),
        0
    );
    assert_eq!(login::prune_expired(&mut tx, ISSUER).await.unwrap(), 100);
    assert_eq!(login::prune_expired(&mut tx, ISSUER).await.unwrap(), 1);
    tx.commit().await.unwrap();
    assert_eq!(
        sqlx::query("SELECT count(*) FROM identity.renewal_credentials")
            .fetch_one(&mut owner)
            .await
            .unwrap()
            .get::<i64, _>(0),
        0
    );
}

async fn wait_blocked(observer: &mut Client, pid: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if sqlx::query("SELECT cardinality(pg_blocking_pids($1))>0")
                .bind(&pid)
                .fetch_one(&mut *observer)
                .await
                .unwrap()
                .get::<bool, _>(0)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the second writer waits for the principal lock");
}
#[sqlx::test(migrations = false)]
async fn simultaneous_rotation_and_logout_cannot_leave_a_usable_successor(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let db = wamn_test_postgres::url(&pool);
    test_database::system(db.as_str()).await;
    let mut first = connect(db.as_str(), "wamn_system").await;
    let mut second = connect(db.as_str(), "wamn_system").await;
    let mut observer = connect(db.as_str(), "wamn_system").await;
    let user = user(&mut first).await;
    let issued = create(&mut first, &user).await;
    let mut tx = first.begin().await.unwrap();
    let replacement = login::rotate_login(
        &mut tx,
        ISSUER,
        AUDIENCE,
        issued.secret(),
        &mut AuthDrops::default(),
    )
    .await
    .unwrap()
    .unwrap();
    let pid: i32 = sqlx::query("SELECT pg_backend_pid()")
        .fetch_one(&mut second)
        .await
        .unwrap()
        .get(0);
    let racing = tokio::spawn(async move { rotate(&mut second, issued.secret()).await });
    wait_blocked(&mut observer, pid).await;
    tx.commit().await.unwrap();
    assert!(racing.await.unwrap().is_none());
    assert!(rotate(&mut first, replacement.secret()).await.is_none());

    let issued = create(&mut first, &user).await;
    let mut second = connect(db.as_str(), "wamn_system").await;
    let pid: i32 = sqlx::query("SELECT pg_backend_pid()")
        .fetch_one(&mut second)
        .await
        .unwrap()
        .get(0);
    let mut tx = first.begin().await.unwrap();
    let replacement = login::rotate_login(
        &mut tx,
        ISSUER,
        AUDIENCE,
        issued.secret(),
        &mut AuthDrops::default(),
    )
    .await
    .unwrap()
    .unwrap();
    let racing = tokio::spawn(async move {
        let mut tx = second.begin().await.unwrap();
        login::revoke_login(
            &mut tx,
            ISSUER,
            AUDIENCE,
            issued.secret(),
            &mut AuthDrops::default(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    });
    wait_blocked(&mut observer, pid).await;
    tx.commit().await.unwrap();
    racing.await.unwrap();
    assert!(rotate(&mut first, replacement.secret()).await.is_none());
}

#[sqlx::test(migrations = false)]
async fn disable_waits_for_issuance_and_reenable_cannot_restore_a_family(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let db = wamn_test_postgres::url(&pool);
    test_database::system(db.as_str()).await;
    let mut first = connect(db.as_str(), "wamn_system").await;
    let mut second = connect(db.as_str(), "wamn_system").await;
    let mut observer = connect(db.as_str(), "wamn_system").await;
    let user = user(&mut first).await;
    let pid: i32 = sqlx::query("SELECT pg_backend_pid()")
        .fetch_one(&mut second)
        .await
        .unwrap()
        .get(0);
    let mut tx = first.begin().await.unwrap();
    let issued = login::create_login(&mut tx, &user, ISSUER, AUDIENCE)
        .await
        .unwrap()
        .unwrap();
    let principal = user;
    let racing = tokio::spawn(async move {
        disable_principal(&mut second, &principal, &mut AuthDrops::default())
            .await
            .unwrap();
    });
    wait_blocked(&mut observer, pid).await;
    tx.commit().await.unwrap();
    racing.await.unwrap();
    assert!(rotate(&mut first, issued.secret()).await.is_none());
    sqlx::query(
        "UPDATE identity.principals SET status='active',disabled_at=NULL WHERE id=$1::text::uuid",
    )
    .bind(&user.to_string().as_str())
    .execute(&mut first)
    .await
    .map(|result| result.rows_affected())
    .unwrap();
    assert!(rotate(&mut first, issued.secret()).await.is_none());
    let fresh = create(&mut first, &user).await;
    let mut tx = first.begin().await.unwrap();
    login::revoke_all(&mut tx, &user, &mut AuthDrops::default())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(rotate(&mut first, fresh.secret()).await.is_none());
}
