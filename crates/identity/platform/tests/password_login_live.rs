//! Renewal storage, revocation races and the scoped issuer on real PostgreSQL.

use sha2::{Digest as _, Sha256};
use tokio_postgres::Client;
use wamn_control_provision::{identity_issuer::grant_identity_issuer_surface_sql, test_database};
use wamn_platform_identity::password_login::{self as login, Renewal};
use wamn_platform_identity::{PrincipalId, create_human, disable_principal};

const ISSUER: &str = "https://identity.example.invalid";
const AUDIENCE: &str = "demo:widgets:dev:a1b2c3d4";
const ACTOR: &str = "770df186-ac15-579e-b46b-c297cae2011b";

async fn connect(url: &str, role: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
        .batch_execute(&format!("SET ROLE {role}"))
        .await
        .unwrap();
    client
        .execute("SELECT set_config('app.user_id',$1,false)", &[&ACTOR])
        .await
        .unwrap();
    client
}
async fn person(client: &Client) -> PrincipalId {
    create_human(
        client,
        "alice@example.invalid",
        "alice@example.invalid",
        "Alice",
    )
    .await
    .unwrap()
    .id()
    .clone()
}
async fn create(client: &mut Client, person: &PrincipalId) -> Renewal {
    let tx = client.transaction().await.unwrap();
    let issued = login::create_login(&tx, person, ISSUER, AUDIENCE)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    issued
}
async fn rotate(client: &mut Client, secret: &str) -> Option<Renewal> {
    let tx = client.transaction().await.unwrap();
    let result = login::rotate_login(&tx, ISSUER, AUDIENCE, secret)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    result
}

#[tokio::test]
async fn scoped_rotation_preserves_deadline_and_replay_revokes_successor() {
    let _lock = wamn_test_postgres::lock();
    let db = test_database::system();
    let owner = connect(db.url(), "wamn_system").await;
    let person = person(&owner).await;
    db.execute(&[&grant_identity_issuer_surface_sql()]).unwrap();
    let mut issuer = connect(db.url(), "wamn_identity_issuer").await;
    let first = create(&mut issuer, &person).await;
    assert_eq!(first.login.expires_at - first.login.authenticated_at, 28800);
    assert!(!format!("{first:?}").contains(first.secret()));
    let stored: String = owner
        .query_one(
            "SELECT row_to_json(c)::text FROM identity.renewal_credentials c",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(!stored.contains(first.secret()));
    let hash: Vec<u8> = owner
        .query_one("SELECT token_hash FROM identity.renewal_credentials", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(hash, Sha256::digest(first.secret()).to_vec());
    let tx = issuer.transaction().await.unwrap();
    assert!(
        login::rotate_login(&tx, "https://other.invalid", AUDIENCE, first.secret())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        login::rotate_login(&tx, ISSUER, "other-instance", first.secret())
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();
    let tx = issuer.transaction().await.unwrap();
    let rolled_back = login::rotate_login(&tx, ISSUER, AUDIENCE, first.secret())
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
        owner
            .query_one("SELECT count(*) FROM identity.renewal_credentials", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );
    // The issuer can rotate and revoke, but cannot rewrite account binding or deadlines.
    let error = issuer
        .batch_execute(
            "UPDATE identity.password_logins SET expires_at=expires_at+interval '1 hour'",
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)
    );
    let error = issuer
        .batch_execute("DELETE FROM identity.renewal_credentials")
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)
    );
    let tx = issuer.transaction().await.unwrap();
    assert_eq!(login::prune_expired(&tx, ISSUER).await.unwrap(), 0);
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn inactivity_absolute_expiry_and_bounded_cleanup() {
    let _lock = wamn_test_postgres::lock();
    let db = test_database::system();
    let mut owner = connect(db.url(), "wamn_system").await;
    let person = person(&owner).await;
    let idle = create(&mut owner, &person).await;
    owner.execute("UPDATE identity.password_logins SET authenticated_at=authenticated_at-interval '1 hour',expires_at=expires_at-interval '1 hour',renewal_expires_at=clock_timestamp()-interval '1 second' WHERE id=$1::text::uuid",&[&idle.login.id]).await.unwrap();
    assert!(rotate(&mut owner, idle.secret()).await.is_none());
    let final_credential = create(&mut owner, &person).await;
    owner.execute("UPDATE identity.password_logins SET authenticated_at=authenticated_at-interval '7 hours 55 minutes',expires_at=expires_at-interval '7 hours 55 minutes',renewal_expires_at=expires_at-interval '7 hours 55 minutes' WHERE id=$1::text::uuid",&[&final_credential.login.id]).await.unwrap();
    let last = rotate(&mut owner, final_credential.secret()).await.unwrap();
    assert!(owner.query_one("SELECT renewal_expires_at=expires_at FROM identity.password_logins WHERE id=$1::text::uuid",&[&last.login.id]).await.unwrap().get::<_,bool>(0));
    for _ in 0..99 {
        create(&mut owner, &person).await;
    }
    owner.batch_execute("UPDATE identity.password_logins SET authenticated_at=authenticated_at-interval '9 hours',expires_at=expires_at-interval '9 hours',renewal_expires_at=renewal_expires_at-interval '9 hours'").await.unwrap();
    assert!(rotate(&mut owner, last.secret()).await.is_none());
    db.execute(&[&grant_identity_issuer_surface_sql()]).unwrap();
    let mut issuer = connect(db.url(), "wamn_identity_issuer").await;
    let tx = issuer.transaction().await.unwrap();
    assert_eq!(
        login::prune_expired(&tx, "https://other.invalid")
            .await
            .unwrap(),
        0
    );
    assert_eq!(login::prune_expired(&tx, ISSUER).await.unwrap(), 100);
    assert_eq!(login::prune_expired(&tx, ISSUER).await.unwrap(), 1);
    tx.commit().await.unwrap();
    assert_eq!(
        owner
            .query_one("SELECT count(*) FROM identity.renewal_credentials", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

async fn wait_blocked(observer: &Client, pid: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if observer
                .query_one("SELECT cardinality(pg_blocking_pids($1))>0", &[&pid])
                .await
                .unwrap()
                .get::<_, bool>(0)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the second writer waits for the principal lock");
}
#[tokio::test]
async fn simultaneous_rotation_and_logout_cannot_leave_a_usable_successor() {
    let _lock = wamn_test_postgres::lock();
    let db = test_database::system();
    let mut first = connect(db.url(), "wamn_system").await;
    let mut second = connect(db.url(), "wamn_system").await;
    let observer = connect(db.url(), "wamn_system").await;
    let person = person(&first).await;
    let issued = create(&mut first, &person).await;
    let tx = first.transaction().await.unwrap();
    let replacement = login::rotate_login(&tx, ISSUER, AUDIENCE, issued.secret())
        .await
        .unwrap()
        .unwrap();
    let pid: i32 = second
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let racing = tokio::spawn(async move { rotate(&mut second, issued.secret()).await });
    wait_blocked(&observer, pid).await;
    tx.commit().await.unwrap();
    assert!(racing.await.unwrap().is_none());
    assert!(rotate(&mut first, replacement.secret()).await.is_none());

    let issued = create(&mut first, &person).await;
    let mut second = connect(db.url(), "wamn_system").await;
    let pid: i32 = second
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let tx = first.transaction().await.unwrap();
    let replacement = login::rotate_login(&tx, ISSUER, AUDIENCE, issued.secret())
        .await
        .unwrap()
        .unwrap();
    let racing = tokio::spawn(async move {
        let tx = second.transaction().await.unwrap();
        login::revoke_login(&tx, ISSUER, AUDIENCE, issued.secret())
            .await
            .unwrap();
        tx.commit().await.unwrap();
    });
    wait_blocked(&observer, pid).await;
    tx.commit().await.unwrap();
    racing.await.unwrap();
    assert!(rotate(&mut first, replacement.secret()).await.is_none());
}

#[tokio::test]
async fn disable_waits_for_issuance_and_reenable_cannot_restore_a_family() {
    let _lock = wamn_test_postgres::lock();
    let db = test_database::system();
    let mut first = connect(db.url(), "wamn_system").await;
    let second = connect(db.url(), "wamn_system").await;
    let observer = connect(db.url(), "wamn_system").await;
    let person = person(&first).await;
    let pid: i32 = second
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let tx = first.transaction().await.unwrap();
    let issued = login::create_login(&tx, &person, ISSUER, AUDIENCE)
        .await
        .unwrap()
        .unwrap();
    let principal = person.clone();
    let racing = tokio::spawn(async move {
        disable_principal(&second, &principal).await.unwrap();
    });
    wait_blocked(&observer, pid).await;
    tx.commit().await.unwrap();
    racing.await.unwrap();
    assert!(rotate(&mut first, issued.secret()).await.is_none());
    first.execute("UPDATE identity.principals SET status='active',disabled_at=NULL WHERE id=$1::text::uuid",&[&person.as_str()]).await.unwrap();
    assert!(rotate(&mut first, issued.secret()).await.is_none());
    let fresh = create(&mut first, &person).await;
    let tx = first.transaction().await.unwrap();
    login::revoke_all(&tx, &person).await.unwrap();
    tx.commit().await.unwrap();
    assert!(rotate(&mut first, fresh.secret()).await.is_none());
}
