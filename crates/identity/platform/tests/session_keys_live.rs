//! Test of the actual signing-key lifecycle on a test PostgreSQL database.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wamn_control_provision::SYSTEM_SCHEMA_SQL;
use wamn_identity_protocol::keys::PublicSessionKey;
use wamn_identity_protocol::test_util::{Audience, verify};
use wamn_identity_protocol::token::SessionClaims;
use wamn_platform_identity::session_keys::{
    activate_session_key, publish_session_key, remove_compromised_session_key, retire_session_keys,
    session_jwks,
};
use wamn_platform_identity::session_token::{IssuedSessionToken, sign_session_token};

const ISSUER: &str = "https://identity.lifecycle.internal";
const OTHER_ISSUER: &str = "https://other.lifecycle.internal";

#[sqlx::test(migrations = false)]
async fn session_key_lifecycle_on_postgres(pool: sqlx::PgPool) {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let (url, mut admin) = prepare_database(test_database.as_str()).await;

    let first = publish_session_key(&mut admin, ISSUER).await.unwrap();
    let second = publish_session_key(&mut admin, ISSUER).await.unwrap();
    let third = publish_session_key(&mut admin, ISSUER).await.unwrap();
    let other = publish_session_key(&mut admin, OTHER_ISSUER).await.unwrap();
    let actor = wamn_control_provision::PlatformComponent::Provisioning
        .principal_id()
        .to_string();
    for relation in ["identity.session_keys", "identity.session_signing_state"] {
        wamn_test_infrastructure::record_history::assert_stamp(
            &mut admin, relation, "true", &actor, &actor,
        )
        .await;
    }
    assert_ne!(first.kid, second.kid);
    assert_ne!(first.kid, third.kid);
    assert_ne!(second.kid, third.kid);
    assert_eq!(
        session_jwks(&mut admin, ISSUER).await.unwrap().keys.len(),
        3
    );
    assert!(
        sign_session_token(&mut admin, claims(ISSUER), now())
            .await
            .is_err(),
        "publication must not implicitly activate"
    );
    assert!(
        activate_session_key(&mut admin, ISSUER, &other.kid)
            .await
            .is_err(),
        "another issuer's publication cannot be selected"
    );
    activate_session_key(&mut admin, ISSUER, &first.kid)
        .await
        .unwrap();
    let before = table_versions(&mut admin).await;
    let initial = sign_session_token(&mut admin, claims(ISSUER), now())
        .await
        .unwrap();
    assert_token(&initial, &first).await;
    assert_eq!(
        table_versions(&mut admin).await,
        before,
        "signing made a logical per-token write"
    );
    assert!(
        !format!("{initial:?}").contains(initial.token()),
        "Debug exposed the token"
    );
    assert!(
        sign_session_token(&mut admin, claims(ISSUER), now() - 901)
            .await
            .is_err(),
        "late validation evidence was signed"
    );

    let raced = race_signing_and_rotation(&url, &mut admin, &second).await;
    assert_token(&raced, &first).await;
    let cutoff = key_state(&mut admin, &first.kid).await;
    assert!(cutoff.0.is_some());
    assert!(cutoff.1, "rotation retained old private material");
    assert!(
        raced.claims().iat * 1_000_000 <= cutoff.0.unwrap(),
        "old token was signed beyond its cutoff"
    );
    assert_eq!(
        retire_session_keys(&mut admin, ISSUER).await.unwrap(),
        0,
        "rotation removed unexpired public evidence"
    );
    assert!(
        session_jwks(&mut admin, ISSUER)
            .await
            .unwrap()
            .keys
            .contains(&first)
    );
    activate_session_key(&mut admin, ISSUER, &second.kid)
        .await
        .unwrap();
    assert_eq!(
        key_state(&mut admin, &first.kid).await,
        cutoff,
        "repeat activation reset cutoff"
    );
    assert!(
        activate_session_key(&mut admin, ISSUER, &first.kid)
            .await
            .is_err(),
        "retired immutable generation was reactivated"
    );
    assert_token(
        &sign_session_token(&mut admin, claims(ISSUER), now())
            .await
            .unwrap(),
        &second,
    )
    .await;

    crash_rotation(&url, &mut admin, &third).await;
    assert_eq!(
        key_state(&mut admin, &second.kid).await,
        (None, false),
        "crashed transaction partially retired its active generation"
    );
    let mut restarted = connect(&url).await;
    assert_eq!(
        key_state(&mut restarted, &first.kid).await,
        cutoff,
        "reconnecting lost or restarted the durable cutoff"
    );
    assert_token(
        &sign_session_token(&mut restarted, claims(ISSUER), now())
            .await
            .unwrap(),
        &second,
    )
    .await;
    activate_session_key(&mut restarted, ISSUER, &third.kid)
        .await
        .unwrap();
    assert_token(
        &sign_session_token(&mut restarted, claims(ISSUER), now())
            .await
            .unwrap(),
        &third,
    )
    .await;

    assert!(
        remove_compromised_session_key(&mut restarted, ISSUER, &third.kid)
            .await
            .unwrap()
    );
    assert!(
        !session_jwks(&mut restarted, ISSUER)
            .await
            .unwrap()
            .keys
            .contains(&third)
    );
    assert_eq!(
        stored_count(&mut admin, ISSUER, &third.kid).await,
        0,
        "compromise left private or public key material in the key table"
    );
    assert!(
        sign_session_token(&mut restarted, claims(ISSUER), now())
            .await
            .is_err(),
        "active compromise silently fell back to another key"
    );
    assert!(
        remove_compromised_session_key(&mut restarted, ISSUER, &second.kid)
            .await
            .unwrap()
    );
    assert!(
        !remove_compromised_session_key(&mut restarted, ISSUER, &second.kid)
            .await
            .unwrap()
    );
    assert!(
        session_jwks(&mut restarted, ISSUER)
            .await
            .unwrap()
            .keys
            .contains(&first)
    );
    assert_eq!(
        session_jwks(&mut restarted, OTHER_ISSUER)
            .await
            .unwrap()
            .keys,
        vec![other]
    );

    // Age only the already-retired fixture row; do not wait fifteen minutes or
    // replace the production clock/retirement query with a mock.
    sqlx::raw_sql("BEGIN; SET LOCAL app.user_id = '770df186-ac15-579e-b46b-c297cae2011b'")
        .execute(&mut admin)
        .await
        .map(drop)
        .unwrap();
    sqlx::query("UPDATE identity.session_keys SET signing_cutoff = clock_timestamp() - interval '931 seconds' \
         WHERE issuer = $1 AND kid::text = $2").bind(&ISSUER).bind(&first.kid).execute(&mut admin).await.map(|result| result.rows_affected()).unwrap();
    sqlx::raw_sql("COMMIT")
        .execute(&mut admin)
        .await
        .map(drop)
        .unwrap();
    assert!(
        session_jwks(&mut restarted, ISSUER)
            .await
            .unwrap()
            .keys
            .is_empty(),
        "expired public evidence remained visible before cleanup"
    );
    assert_eq!(
        retire_session_keys(&mut restarted, ISSUER).await.unwrap(),
        1
    );
    assert_eq!(stored_count(&mut admin, ISSUER, &first.kid).await, 0);
    let replacement = publish_session_key(&mut restarted, ISSUER).await.unwrap();
    assert!(
        ![&first.kid, &second.kid, &third.kid].contains(&&replacement.kid),
        "new publication recycled a deleted generation"
    );
    assert!(
        sign_session_token(&mut restarted, claims(ISSUER), now())
            .await
            .is_err(),
        "publication recovered a compromised active key implicitly"
    );

    sqlx::raw_sql("DROP SCHEMA identity CASCADE; DROP SCHEMA provisioning CASCADE; DROP SCHEMA registry CASCADE;").execute(&mut admin).await.map(drop).expect("remove owned platform test schemas");
}

#[sqlx::test(migrations = false)]
async fn signer_backend_loss_before_commit_does_not_issue_token(pool: sqlx::PgPool) {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    tokio::time::timeout(Duration::from_secs(30), async {
        let (url, mut admin) = prepare_database(test_database.as_str()).await;
        let first = publish_session_key(&mut admin, ISSUER).await.unwrap();
        let second = publish_session_key(&mut admin, ISSUER).await.unwrap();
        activate_session_key(&mut admin, ISSUER, &first.kid).await.unwrap();
        assert_token(
            &sign_session_token(&mut admin, claims(ISSUER), now()).await.unwrap(),
            &first,
        ).await;

        let mut blocker = connect(&url).await;
        let blocker_pid = pid(&mut blocker).await;
        let mut transaction = blocker.begin().await.unwrap();
        sqlx::raw_sql("LOCK TABLE identity.session_signing_state IN ACCESS EXCLUSIVE MODE").execute(&mut *transaction).await.unwrap();
        let mut signer = connect(&url).await;
        let signer_pid = pid(&mut signer).await;
        let signing = sign_session_token(&mut signer, claims(ISSUER), now());
        tokio::pin!(signing);
        tokio::select! {
            _ = &mut signing => panic!("signer passed the blocked transaction"),
            () = wait_blocked(&mut admin,signer_pid,blocker_pid) => {}
        }
        assert!(sqlx::query_scalar::<_,bool>("SELECT pg_terminate_backend($1)").bind(signer_pid).fetch_one(&mut admin).await.unwrap());
        assert!(tokio::time::timeout(Duration::from_secs(5), &mut signing).await.unwrap().is_err(),"failed signing transaction released a token");
        transaction.rollback().await.unwrap();
        activate_session_key(&mut admin,ISSUER,&second.kid).await.unwrap();
        assert_token(&sign_session_token(&mut admin,claims(ISSUER),now()).await.unwrap(),&second).await;
        sqlx::raw_sql("DROP SCHEMA identity CASCADE; DROP SCHEMA provisioning CASCADE; DROP SCHEMA registry CASCADE;").execute(&mut admin).await.map(drop).expect("remove owned platform test schemas");
    }).await.expect("signer backend-loss test exceeded its bounded deadline");
}

// Each test passes its own test database and holds the process lock, because
// the system schema creates the cluster-wide wamn_system and wamn_db_owner roles.
async fn prepare_database(url: &str) -> (String, Client) {
    let mut admin = connect(url).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DROP SCHEMA IF EXISTS identity CASCADE; DROP SCHEMA IF EXISTS provisioning CASCADE; \
         DROP SCHEMA IF EXISTS registry CASCADE; \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') \
         THEN CREATE ROLE wamn_system; END IF; END $$;")
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("prepare empty platform schemas");
    sqlx::raw_sql(SYSTEM_SCHEMA_SQL)
        .execute(&mut admin)
        .await
        .map(drop)
        .expect("apply production system schema");
    (url.to_owned(), admin)
}

async fn race_signing_and_rotation(
    url: &str,
    observer: &mut Client,
    next: &PublicSessionKey,
) -> IssuedSessionToken {
    let mut blocker = connect(url).await;
    let mut signer = connect(url).await;
    let mut rotator = connect(url).await;
    let blocker_pid = pid(&mut blocker).await;
    let signer_pid = pid(&mut signer).await;
    let rotator_pid = pid(&mut rotator).await;
    let mut blocker_tx = blocker.begin().await.unwrap();
    sqlx::raw_sql("LOCK TABLE identity.session_keys IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker_tx)
        .await
        .map(drop)
        .unwrap();
    let signing =
        tokio::spawn(async move { sign_session_token(&mut signer, claims(ISSUER), now()).await });
    // The actual signer has its issuer SHARE lock and is waiting for key bytes.
    wait_blocked(observer, signer_pid, blocker_pid).await;
    let kid = next.kid.clone();
    let rotation =
        tokio::spawn(async move { activate_session_key(&mut rotator, ISSUER, &kid).await });
    wait_blocked(observer, rotator_pid, signer_pid).await;
    blocker_tx.commit().await.unwrap();
    let token = tokio::time::timeout(Duration::from_secs(5), signing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), rotation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    token
}

async fn crash_rotation(url: &str, observer: &mut Client, next: &PublicSessionKey) {
    // Pause the real flip after both its private-key erasure and state update,
    // then kill only that test-owned backend before either write can commit.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("CREATE FUNCTION identity.pause_session_flip() RETURNS trigger LANGUAGE plpgsql AS \
         $$ BEGIN PERFORM pg_sleep(30); RETURN NEW; END $$; \
         CREATE TRIGGER pause_session_flip AFTER UPDATE ON identity.session_signing_state \
         FOR EACH ROW EXECUTE FUNCTION identity.pause_session_flip();")
            .to_string(),
    ))
    .execute(&mut *observer)
    .await
    .map(drop)
    .unwrap();
    let mut rotator = connect(url).await;
    let rotator_pid = pid(&mut rotator).await;
    let kid = next.kid.clone();
    let rotation =
        tokio::spawn(async move { activate_session_key(&mut rotator, ISSUER, &kid).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let sleeping: bool = sqlx::query("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event='PgSleep')").bind(&rotator_pid).fetch_one(&mut *observer).await.unwrap().get(0);
            if sleeping { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("real rotation did not reach its uncommitted cutoff/state write");
    let terminated: bool = sqlx::query("SELECT pg_terminate_backend($1)")
        .bind(&rotator_pid)
        .fetch_one(&mut *observer)
        .await
        .unwrap()
        .get(0);
    assert!(
        terminated,
        "test did not terminate its owned rotation backend"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(5), rotation)
            .await
            .unwrap()
            .unwrap()
            .is_err(),
        "terminated rotation unexpectedly committed"
    );
    sqlx::raw_sql("DROP TRIGGER pause_session_flip ON identity.session_signing_state; DROP FUNCTION identity.pause_session_flip();").execute(&mut *observer).await.map(drop).unwrap();
}

async fn wait_blocked(observer: &mut Client, waiting: i32, blocker: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let is_blocked: bool = sqlx::query("SELECT $2=ANY(pg_blocking_pids($1))")
                .bind(&waiting)
                .bind(&blocker)
                .fetch_one(&mut *observer)
                .await
                .unwrap()
                .get(0);
            if is_blocked {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("production key operation did not wait on the expected backend lock");
}

async fn connect(url: &str) -> Client {
    let client = wamn_postgres::connect(url)
        .await
        .expect("connect disposable key database");
    // One connection is deliberately terminated to show transaction rollback.

    client
}

async fn pid(client: &mut Client) -> i32 {
    sqlx::query("SELECT pg_backend_pid()")
        .fetch_one(&mut *client)
        .await
        .unwrap()
        .get(0)
}

async fn key_state(client: &mut Client, kid: &str) -> (Option<i64>, bool) {
    let row = sqlx::query(
        "SELECT (extract(epoch FROM signing_cutoff) * 1000000)::bigint, private_pkcs8 IS NULL \
         FROM identity.session_keys WHERE issuer=$1 AND kid::text=$2",
    )
    .bind(&ISSUER)
    .bind(&kid)
    .fetch_one(&mut *client)
    .await
    .unwrap();
    (row.get(0), row.get(1))
}

async fn table_versions(client: &mut Client) -> (String, String) {
    let row = sqlx::query("SELECT (SELECT string_agg(issuer || ':' || kid::text || ':' || xmin::text, ',' ORDER BY issuer,kid) \
         FROM identity.session_keys), (SELECT string_agg(issuer || ':' || xmin::text, ',' ORDER BY issuer) \
         FROM identity.session_signing_state)").fetch_one(&mut *client).await.unwrap();
    (row.get(0), row.get(1))
}

async fn stored_count(client: &mut Client, issuer: &str, kid: &str) -> i64 {
    sqlx::query("SELECT count(*) FROM identity.session_keys WHERE issuer=$1 AND kid::text=$2")
        .bind(&issuer)
        .bind(&kid)
        .fetch_one(&mut *client)
        .await
        .unwrap()
        .get(0)
}

fn claims(issuer: &str) -> SessionClaims {
    SessionClaims {
        iss: issuer.into(),
        sub: "ed7056a9-5639-455f-9640-4678458794c0".parse().unwrap(),
        org: "org-a".into(),
        aud: "environment-id-a-dev".into(),
        roles: vec!["purchase-reader".parse().unwrap()],
        iat: 0,
        exp: 0,
        jti: "fixture-token-id".into(),
        authority: wamn_identity_protocol::token::SessionAuthority::Login(
            "ed7056a9-5639-455f-9640-4678458794c0".parse().unwrap(),
        ),
        csrf: None,
    }
}

async fn assert_token(token: &IssuedSessionToken, key: &PublicSessionKey) {
    assert_eq!(
        jsonwebtoken::decode_header(token.token())
            .unwrap()
            .kid
            .expect("key ID"),
        key.kid
    );
    let verified = verify(
        token.token(),
        key,
        Audience {
            issuer: ISSUER,
            org: "org-a",
            audience: "environment-id-a-dev",
        },
        now(),
    )
    .await
    .unwrap();
    assert_eq!(&verified, token.claims());
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .try_into()
        .unwrap()
}

#[sqlx::test(migrations = false)]
async fn scheduled_rotation_keeps_the_previous_verification_key(pool: sqlx::PgPool) {
    let _serialized = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    let (_, mut admin) = prepare_database(database.as_str()).await;
    let issuer = "https://identity.schedule.internal";
    let interval = Duration::from_secs(3600);
    let first = publish_session_key(&mut admin, issuer).await.unwrap();
    activate_session_key(&mut admin, issuer, &first.kid)
        .await
        .unwrap();
    let initial: String = sqlx::query(sqlx::AssertSqlSafe(
        ("SELECT active_kid::text FROM identity.session_signing_state WHERE issuer=$1").to_string(),
    ))
    .bind(&issuer)
    .fetch_one(&mut admin)
    .await
    .unwrap()
    .get(0);
    wamn_platform_identity::session_keys::rotate_session_keys(&mut admin, issuer, interval)
        .await
        .unwrap();
    let unchanged: String = sqlx::query(sqlx::AssertSqlSafe(
        ("SELECT active_kid::text FROM identity.session_signing_state WHERE issuer=$1").to_string(),
    ))
    .bind(&issuer)
    .fetch_one(&mut admin)
    .await
    .unwrap()
    .get(0);
    assert_eq!(unchanged, initial);
    let mut tx = admin.begin().await.unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(("UPDATE identity.session_keys SET published_at=clock_timestamp()-interval '2 hours' WHERE issuer=$1").to_string())).bind(&issuer).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    wamn_platform_identity::session_keys::rotate_session_keys(&mut admin, issuer, interval)
        .await
        .unwrap();
    let current: String = sqlx::query(sqlx::AssertSqlSafe(
        ("SELECT active_kid::text FROM identity.session_signing_state WHERE issuer=$1").to_string(),
    ))
    .bind(&issuer)
    .fetch_one(&mut admin)
    .await
    .unwrap()
    .get(0);
    assert_ne!(current, initial);
    let public = session_jwks(&mut admin, issuer).await.unwrap();
    assert!(public.keys.iter().any(|key| key.kid == initial));
    assert!(public.keys.iter().any(|key| key.kid == current));
    remove_compromised_session_key(&mut admin, issuer, &current)
        .await
        .unwrap();
    wamn_platform_identity::session_keys::rotate_session_keys(&mut admin, issuer, interval)
        .await
        .unwrap();
    let active: Option<String> = sqlx::query(sqlx::AssertSqlSafe(
        ("SELECT active_kid::text FROM identity.session_signing_state WHERE issuer=$1").to_string(),
    ))
    .bind(&issuer)
    .fetch_one(&mut admin)
    .await
    .unwrap()
    .get(0);
    assert!(active.is_none());
}
