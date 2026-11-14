//! Live test of the lifecycle lock
//! (docs/architecture/overview.md#environment-lifecycle, wamn-snz0.1): a second
//! verb on the same coordinate refuses with "deployment in progress",
//! another coordinate is not blocked, and closing the holder's connection
//! releases the lock.

use sqlx::{Connection as _, Row as _};
use std::time::Duration;
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time::timeout,
};
use wamn_environment::environment::lock::LifecycleLock;
use wamn_registry::Triple;

#[tokio::test]
async fn a_second_verb_refuses_until_the_holder_is_gone() {
    let database = wamn_test_postgres::database();
    let url = database.url();
    let prod = Triple::new("acme", "prod", "wms");

    LifecycleLock::hold(url, &prod, async |_| {
        assert!(
            timeout(Duration::from_secs(7), LifecycleLock::acquire(url, &prod))
                .await?
                .is_err()
        );
        assert!(
            LifecycleLock::acquire_application(url, &prod, "orders")
                .await
                .is_err()
        );
        let other = LifecycleLock::acquire(url, &Triple::new("acme", "dev", "wms")).await?;
        other.release().await;
        Ok(())
    })
    .await
    .unwrap();
    let orders = LifecycleLock::acquire_application(url, &prod, "orders")
        .await
        .unwrap();
    assert!(
        timeout(
            Duration::from_secs(7),
            LifecycleLock::acquire_application(url, &prod, "orders"),
        )
        .await
        .unwrap()
        .is_err()
    );
    let receiving = LifecycleLock::acquire_application(url, &prod, "receiving")
        .await
        .unwrap();
    assert!(LifecycleLock::acquire(url, &prod).await.is_err());
    orders.release().await;
    let retry = LifecycleLock::acquire_application(url, &prod, "orders")
        .await
        .unwrap();
    retry.release().await;
    receiving.release().await;

    // A SQL Job keeps its own shared session after the CLI session disappears.
    let cli = LifecycleLock::acquire_shared(url, &prod).await.unwrap();
    let job = LifecycleLock::acquire_shared(url, &prod).await.unwrap();
    drop(cli);
    assert!(LifecycleLock::acquire(url, &prod).await.is_err());
    let waiting = LifecycleLock::acquire_wait(url, &prod);
    tokio::pin!(waiting);
    assert!(
        timeout(Duration::from_millis(200), &mut waiting)
            .await
            .is_err()
    );
    job.release().await;
    timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .release()
        .await;
}

#[tokio::test]
async fn abandoned_sessions_expire_even_when_a_proxy_keeps_the_socket_open() {
    let database = wamn_test_postgres::database();
    let triple = Triple::new("acme", "prod", "wms");
    let mut tasks = JoinSet::new();
    let mut observer = wamn_postgres::connect(database.url()).await.unwrap();
    tasks.spawn(async move {});

    for application in [None, Some("orders")] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut proxy_url = url::Url::parse(database.url()).unwrap();
        let server_port = proxy_url.port().unwrap();
        proxy_url
            .set_port(Some(listener.local_addr().unwrap().port()))
            .unwrap();
        let (stopped, client_stopped) = tokio::sync::oneshot::channel();
        tasks.spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            let mut server = TcpStream::connect(("127.0.0.1", server_port))
                .await
                .unwrap();
            let (mut client_read, mut client_write) = client.split();
            let (mut server_read, mut server_write) = server.split();
            // copy does not forward EOF as a shutdown to the server socket.
            tokio::select! {
                _ = tokio::io::copy(&mut client_read, &mut server_write) => {},
                _ = tokio::io::copy(&mut server_read, &mut client_write) => {},
            }
            stopped.send(()).unwrap();
            std::future::pending::<()>().await;
            drop(server);
        });
        let held = acquire(proxy_url.as_str(), &triple, application).await;
        let pid: i32 = sqlx::query("SELECT pg_backend_pid()")
            .fetch_one(&mut *held.client().await)
            .await
            .unwrap()
            .get(0);
        drop(held);
        timeout(Duration::from_secs(1), client_stopped)
            .await
            .unwrap()
            .unwrap();
        assert!(sqlx::query("SELECT EXISTS (SELECT FROM pg_locks WHERE pid = $1 AND locktype = 'advisory' AND granted)").bind(&pid).fetch_one(&mut observer).await.unwrap().get::<bool, _>(0), "the abandoned server session still holds its lock");

        let recovered = timeout(
            Duration::from_secs(7),
            acquire(database.url(), &triple, application),
        )
        .await
        .expect("an abandoned lock becomes available within the acquisition bound");
        recovered.release().await;
        assert!(
            !sqlx::query("SELECT EXISTS (SELECT FROM pg_stat_activity WHERE pid = $1)")
                .bind(&pid)
                .fetch_one(&mut observer)
                .await
                .unwrap()
                .get::<bool, _>(0),
            "PostgreSQL ends the abandoned session"
        );
    }
}

async fn acquire(url: &str, triple: &Triple, application: Option<&str>) -> LifecycleLock {
    match application {
        Some(application) => LifecycleLock::acquire_application(url, triple, application).await,
        None => LifecycleLock::acquire(url, triple).await,
    }
    .unwrap()
}

#[tokio::test]
async fn lock_loss_cancels_the_protected_operation() -> anyhow::Result<()> {
    let database = wamn_test_postgres::database();
    let triple = Triple::new("acme", "prod", "wms");
    let mut observer = wamn_postgres::connect(database.url()).await?;
    let lock = LifecycleLock::acquire(database.url(), &triple).await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *lock.client().await)
        .await?;
    sqlx::query("SELECT pg_terminate_backend($1)")
        .bind(pid)
        .execute(&mut observer)
        .await?;
    let result = timeout(
        Duration::from_secs(5),
        lock.protect(std::future::pending::<anyhow::Result<()>>()),
    )
    .await?;
    assert!(result.is_err());
    lock.release().await;
    LifecycleLock::acquire(database.url(), &triple)
        .await?
        .release()
        .await;
    Ok(())
}
