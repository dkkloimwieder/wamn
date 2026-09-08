//! Armed, disposable-PostgreSQL proof of the actual signing-key lifecycle.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_postgres::config::{Host, SslMode};
use tokio_postgres::{Client, NoTls};
use wamn_platform_identity::session_keys::{
    PublicSessionKey, activate_session_key, publish_session_key, remove_compromised_session_key,
    retire_session_keys, session_jwks,
};
use wamn_platform_identity::session_token::{
    IssuedSessionToken, SessionClaims, SessionScope, session_key_id, sign_session_token,
    verify_session_token,
};

const ISSUER: &str = "https://identity.lifecycle.internal";
const OTHER_ISSUER: &str = "https://other.lifecycle.internal";
const SYSTEM_SCHEMA: &str = include_str!("../../../../deploy/sql/system-schema.sql");

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an explicitly armed disposable PostgreSQL database"]
async fn session_key_lifecycle_on_postgres() {
    let (url, mut admin) = prepare_database().await;

    let first = publish_session_key(&mut admin, ISSUER).await.unwrap();
    let second = publish_session_key(&mut admin, ISSUER).await.unwrap();
    let third = publish_session_key(&mut admin, ISSUER).await.unwrap();
    let other = publish_session_key(&mut admin, OTHER_ISSUER).await.unwrap();
    assert_ne!(first.kid, second.kid);
    assert_ne!(first.kid, third.kid);
    assert_ne!(second.kid, third.kid);
    assert_eq!(session_jwks(&admin, ISSUER).await.unwrap().keys.len(), 3);
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
    let before = table_versions(&admin).await;
    let initial = sign_session_token(&mut admin, claims(ISSUER), now())
        .await
        .unwrap();
    assert_token(&initial, &first);
    assert_eq!(
        table_versions(&admin).await,
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

    let raced = race_signing_and_rotation(&url, &admin, &second).await;
    assert_token(&raced, &first);
    let cutoff = key_state(&admin, &first.kid).await;
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
        session_jwks(&admin, ISSUER)
            .await
            .unwrap()
            .keys
            .contains(&first)
    );
    activate_session_key(&mut admin, ISSUER, &second.kid)
        .await
        .unwrap();
    assert_eq!(
        key_state(&admin, &first.kid).await,
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
    );

    crash_rotation(&url, &admin, &third).await;
    assert_eq!(
        key_state(&admin, &second.kid).await,
        (None, false),
        "crashed transaction partially retired its active generation"
    );
    let mut restarted = connect(&url).await;
    assert_eq!(
        key_state(&restarted, &first.kid).await,
        cutoff,
        "reconnecting lost or restarted the durable cutoff"
    );
    assert_token(
        &sign_session_token(&mut restarted, claims(ISSUER), now())
            .await
            .unwrap(),
        &second,
    );
    activate_session_key(&mut restarted, ISSUER, &third.kid)
        .await
        .unwrap();
    assert_token(
        &sign_session_token(&mut restarted, claims(ISSUER), now())
            .await
            .unwrap(),
        &third,
    );

    assert!(
        remove_compromised_session_key(&mut restarted, ISSUER, &third.kid)
            .await
            .unwrap()
    );
    assert!(
        !session_jwks(&restarted, ISSUER)
            .await
            .unwrap()
            .keys
            .contains(&third)
    );
    assert_eq!(
        stored_count(&admin, ISSUER, &third.kid).await,
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
        session_jwks(&restarted, ISSUER)
            .await
            .unwrap()
            .keys
            .contains(&first)
    );
    assert_eq!(
        session_jwks(&restarted, OTHER_ISSUER).await.unwrap().keys,
        vec![other]
    );

    // Age only the already-retired fixture row; do not wait fifteen minutes or
    // replace the production clock/retirement query with a mock.
    admin.execute(
        "UPDATE identity.session_keys SET signing_cutoff = clock_timestamp() - interval '931 seconds' \
         WHERE issuer = $1 AND kid::text = $2",
        &[&ISSUER,&first.kid],
    ).await.unwrap();
    assert!(
        session_jwks(&restarted, ISSUER)
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
    assert_eq!(stored_count(&admin, ISSUER, &first.kid).await, 0);
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

    admin.batch_execute("DROP SCHEMA identity CASCADE; DROP SCHEMA provisioning CASCADE; DROP SCHEMA registry CASCADE;")
        .await.expect("remove owned platform test schemas");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an explicitly armed disposable PostgreSQL database; run tests serially"]
async fn signer_backend_loss_before_commit_does_not_issue_token() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (url, mut admin) = prepare_database().await;
        let first = publish_session_key(&mut admin, ISSUER).await.unwrap();
        let second = publish_session_key(&mut admin, ISSUER).await.unwrap();
        activate_session_key(&mut admin, ISSUER, &first.kid).await.unwrap();
        assert_token(
            &sign_session_token(&mut admin, claims(ISSUER), now()).await.unwrap(),
            &first,
        );

        // Both the proxy and the actual PostgreSQL driver abort on test failure.
        let mut tasks = JoinSet::new();
        let (mut signer, commit_held, release_commit) =
            connect_with_held_commit(&url, &mut tasks).await;
        let signer_pid = pid(&signer).await;
        let mut rotator = connect(&url).await;
        let rotator_pid = pid(&rotator).await;
        assert_ne!(signer_pid, pid(&admin).await);
        assert_ne!(signer_pid, rotator_pid);
        let signing = sign_session_token(&mut signer, claims(ISSUER), now());
        tokio::pin!(signing);
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                _ = &mut signing => {
                    panic!("signer returned before its COMMIT reached the wire barrier");
                }
                held = commit_held => held.expect("proxy did not observe the actual signing COMMIT"),
            }
        }).await.expect("production signer did not reach its COMMIT wire barrier");

        // The production signer has already called ring, but COMMIT has not
        // reached PostgreSQL. Its real SHARE lock must still block activation.
        let rotation = activate_session_key(&mut rotator, ISSUER, &second.kid);
        tokio::pin!(rotation);
        tokio::select! {
            _ = &mut rotation => panic!("rotation passed the uncommitted signer"),
            () = wait_blocked(&admin, rotator_pid, signer_pid) => {}
        }
        let terminated: bool = admin
            .query_one("SELECT pg_terminate_backend($1)", &[&signer_pid])
            .await.unwrap().get(0);
        assert!(terminated, "test did not terminate its owned signer backend");
        tokio::time::timeout(Duration::from_secs(5), &mut rotation)
            .await.expect("rotation did not finish after signer termination")
            .expect("generation rotation did not commit");
        let cutoff = key_state(&admin, &first.kid).await;
        assert!(cutoff.0.is_some() && cutoff.1, "rotation did not retire the old private key");

        // Forward the held COMMIT only after rotation. A dead backend cannot
        // acknowledge it, and an internally produced signature must not escape.
        release_commit.send(()).expect("proxy abandoned the held COMMIT");
        assert!(
            tokio::time::timeout(Duration::from_secs(5), &mut signing)
                .await.expect("terminated signer did not return").is_err(),
            "signer released a token without a successful transaction commit"
        );
        assert_token(
            &sign_session_token(&mut admin, claims(ISSUER), now()).await.unwrap(),
            &second,
        );
        tasks.shutdown().await;
        admin.batch_execute("DROP SCHEMA identity CASCADE; DROP SCHEMA provisioning CASCADE; DROP SCHEMA registry CASCADE;")
            .await.expect("remove owned platform test schemas");
    }).await.expect("signer backend-loss proof exceeded its bounded deadline");
}

async fn connect_with_held_commit(
    url: &str,
    tasks: &mut JoinSet<()>,
) -> (Client, oneshot::Receiver<()>, oneshot::Sender<()>) {
    let mut config: tokio_postgres::Config = url.parse().expect("parse disposable PostgreSQL URL");
    config.ssl_mode(SslMode::Disable);
    let [Host::Tcp(host)] = config.get_hosts() else {
        panic!("wire proof requires one TCP PostgreSQL host");
    };
    assert!(
        config.get_hostaddrs().is_empty(),
        "wire proof requires host, not hostaddr"
    );
    assert!(
        config.get_ports().len() <= 1,
        "wire proof requires one PostgreSQL port"
    );
    let port = config.get_ports().first().copied().unwrap_or(5432);
    let upstream = TcpStream::connect((host.as_str(), port))
        .await
        .expect("connect proxy upstream");
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind owned wire proxy");
    let downstream = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (frontend, _) = listener.accept().await.unwrap();
    let (held_tx, held_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    tasks.spawn(async move {
        let (mut upstream_read, mut upstream_write) = upstream.into_split();
        let (mut frontend_read, mut frontend_write) = frontend.into_split();
        // Backend bytes include key material. Relay them without inspection,
        // logging, or persistence. Termination errors are expected in this test.
        let _ = tokio::join!(
            relay_until_commit(&mut frontend_read, &mut upstream_write, held_tx, release_rx),
            async {
                tokio::io::copy(&mut upstream_read, &mut frontend_write).await?;
                frontend_write.shutdown().await
            },
        );
    });
    let (client, connection) = config
        .connect_raw(downstream, NoTls)
        .await
        .expect("authenticate real PostgreSQL client through wire proxy");
    tasks.spawn(async move {
        let _ = connection.await;
    });
    (client, held_rx, release_tx)
}

async fn relay_until_commit(
    frontend: &mut tokio::net::tcp::OwnedReadHalf,
    upstream: &mut tokio::net::tcp::OwnedWriteHalf,
    held: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
) -> std::io::Result<()> {
    // Startup has no type byte. Subsequent messages use type + inclusive length.
    // Forward authentication and queries unchanged; never record their payloads.
    let length = frontend.read_u32().await?;
    assert!(
        (8..=65_536).contains(&length),
        "unexpected PostgreSQL startup length"
    );
    let mut payload = vec![0; length as usize - 4];
    frontend.read_exact(&mut payload).await?;
    upstream.write_u32(length).await?;
    upstream.write_all(&payload).await?;
    loop {
        let kind = frontend.read_u8().await?;
        let length = frontend.read_u32().await?;
        assert!(
            (4..=65_536).contains(&length),
            "unexpected PostgreSQL message length"
        );
        payload.resize(length as usize - 4, 0);
        frontend.read_exact(&mut payload).await?;
        if kind == b'Q' && payload == b"COMMIT\0" {
            held.send(()).expect("test stopped observing COMMIT");
            release.await.expect("test did not release COMMIT");
            upstream.write_u8(kind).await?;
            upstream.write_u32(length).await?;
            upstream.write_all(&payload).await?;
            return Ok(());
        }
        upstream.write_u8(kind).await?;
        upstream.write_u32(length).await?;
        upstream.write_all(&payload).await?;
    }
}

// These ignored tests deliberately replace the same disposable schemas. Invoke
// an exact test name or use --test-threads=1 when running the whole test binary.
async fn prepare_database() -> (String, Client) {
    assert_eq!(
        std::env::var("WAMN_SESSION_KEYS_ALLOW_SCHEMA_RESET").as_deref(),
        Ok("1"),
        "arm only an owned disposable database: this test replaces platform schemas"
    );
    let url =
        std::env::var("WAMN_SESSION_KEYS_PG_URL").expect("provide the disposable PostgreSQL URL");
    let admin = connect(&url).await;
    admin
        .batch_execute(
            "DROP SCHEMA IF EXISTS identity CASCADE; DROP SCHEMA IF EXISTS provisioning CASCADE; \
         DROP SCHEMA IF EXISTS registry CASCADE; \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') \
         THEN CREATE ROLE wamn_system; END IF; END $$;",
        )
        .await
        .expect("prepare empty platform schemas");
    admin
        .batch_execute(SYSTEM_SCHEMA)
        .await
        .expect("apply production system schema");
    (url, admin)
}

async fn race_signing_and_rotation(
    url: &str,
    observer: &Client,
    next: &PublicSessionKey,
) -> IssuedSessionToken {
    let mut blocker = connect(url).await;
    let mut signer = connect(url).await;
    let mut rotator = connect(url).await;
    let blocker_pid = pid(&blocker).await;
    let signer_pid = pid(&signer).await;
    let rotator_pid = pid(&rotator).await;
    let blocker_tx = blocker.transaction().await.unwrap();
    blocker_tx
        .batch_execute("LOCK TABLE identity.session_keys IN ACCESS EXCLUSIVE MODE")
        .await
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

async fn crash_rotation(url: &str, observer: &Client, next: &PublicSessionKey) {
    // Pause the real flip after both its private-key erasure and state update,
    // then kill only that test-owned backend before either write can commit.
    observer
        .batch_execute(
            "CREATE FUNCTION identity.pause_session_flip() RETURNS trigger LANGUAGE plpgsql AS \
         $$ BEGIN PERFORM pg_sleep(30); RETURN NEW; END $$; \
         CREATE TRIGGER pause_session_flip AFTER UPDATE ON identity.session_signing_state \
         FOR EACH ROW EXECUTE FUNCTION identity.pause_session_flip();",
        )
        .await
        .unwrap();
    let mut rotator = connect(url).await;
    let rotator_pid = pid(&rotator).await;
    let kid = next.kid.clone();
    let rotation =
        tokio::spawn(async move { activate_session_key(&mut rotator, ISSUER, &kid).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let sleeping: bool = observer.query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event='PgSleep')",
                &[&rotator_pid],
            ).await.unwrap().get(0);
            if sleeping { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("real rotation did not reach its uncommitted cutoff/state write");
    let terminated: bool = observer
        .query_one("SELECT pg_terminate_backend($1)", &[&rotator_pid])
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
    observer.batch_execute(
        "DROP TRIGGER pause_session_flip ON identity.session_signing_state; DROP FUNCTION identity.pause_session_flip();",
    ).await.unwrap();
}

async fn wait_blocked(observer: &Client, waiting: i32, blocker: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = observer
                .query_one("SELECT $2=ANY(pg_blocking_pids($1))", &[&waiting, &blocker])
                .await
                .unwrap()
                .get(0);
            if blocked {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("production key operation did not wait on the expected backend lock");
}

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect disposable key database");
    // One connection is deliberately terminated to prove transaction rollback.
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

async fn pid(client: &Client) -> i32 {
    client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0)
}

async fn key_state(client: &Client, kid: &str) -> (Option<i64>, bool) {
    let row = client
        .query_one(
            "SELECT (extract(epoch FROM signing_cutoff) * 1000000)::bigint, private_pkcs8 IS NULL \
         FROM identity.session_keys WHERE issuer=$1 AND kid::text=$2",
            &[&ISSUER, &kid],
        )
        .await
        .unwrap();
    (row.get(0), row.get(1))
}

async fn table_versions(client: &Client) -> (String, String) {
    let row = client.query_one(
        "SELECT (SELECT string_agg(issuer || ':' || kid::text || ':' || xmin::text, ',' ORDER BY issuer,kid) \
         FROM identity.session_keys), (SELECT string_agg(issuer || ':' || xmin::text, ',' ORDER BY issuer) \
         FROM identity.session_signing_state)", &[],
    ).await.unwrap();
    (row.get(0), row.get(1))
}

async fn stored_count(client: &Client, issuer: &str, kid: &str) -> i64 {
    client
        .query_one(
            "SELECT count(*) FROM identity.session_keys WHERE issuer=$1 AND kid::text=$2",
            &[&issuer, &kid],
        )
        .await
        .unwrap()
        .get(0)
}

fn claims(issuer: &str) -> SessionClaims {
    SessionClaims {
        iss: issuer.into(),
        sub: "ed7056a9-5639-455f-9640-4678458794c0".into(),
        org: "org-a".into(),
        aud: "environment-id-a-dev".into(),
        roles: vec!["purchase-reader".into()],
        iat: 0,
        exp: 0,
        jti: "fixture-token-id".into(),
    }
}

fn assert_token(token: &IssuedSessionToken, key: &PublicSessionKey) {
    assert_eq!(session_key_id(token.token()).unwrap(), key.kid);
    let verified = verify_session_token(
        token.token(),
        key,
        SessionScope {
            issuer: ISSUER,
            org: "org-a",
            audience: "environment-id-a-dev",
        },
        now(),
    )
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
