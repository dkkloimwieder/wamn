//! Live test of the lifecycle lock
//! (docs/architecture/overview.md#environment-lifecycle, wamn-snz0.1): a second
//! verb on the same coordinate refuses with "deployment in progress",
//! another coordinate is not blocked, and closing the holder's connection
//! releases the lock.

use std::time::Duration;
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time::timeout,
};
use wamn_control_registry::Triple;
use wamn_environment_control::environment::lock::{DEPLOYMENT_IN_PROGRESS, LifecycleLock};

#[tokio::test]
async fn a_second_verb_refuses_until_the_holder_is_gone() {
    let database = wamn_test_postgres::database();
    let url = database.url();
    let prod = Triple::new("acme", "wms", "prod");

    LifecycleLock::hold(url, &prod, async |_| {
        let refused = timeout(Duration::from_secs(7), LifecycleLock::acquire(url, &prod))
            .await?
            .expect_err("a live holder survives the idle timeout and refuses a second verb");
        assert!(
            refused.to_string().starts_with(DEPLOYMENT_IN_PROGRESS),
            "{refused:#}"
        );
        let other = LifecycleLock::acquire(url, &Triple::new("acme", "wms", "dev")).await?;
        other.release().await;
        Ok(())
    })
    .await
    .unwrap();
    let held = LifecycleLock::acquire(url, &prod).await.unwrap();

    let orders = LifecycleLock::acquire_application(url, &prod, "orders")
        .await
        .unwrap();
    let refused = timeout(
        Duration::from_secs(7),
        LifecycleLock::acquire_application(url, &prod, "orders"),
    )
    .await
    .unwrap()
    .expect_err("a live application holder survives the idle timeout");
    assert!(
        refused.to_string().starts_with(DEPLOYMENT_IN_PROGRESS),
        "{refused:#}"
    );
    let receiving = LifecycleLock::acquire_application(url, &prod, "receiving")
        .await
        .unwrap();
    orders.release().await;
    let retry = LifecycleLock::acquire_application(url, &prod, "orders")
        .await
        .unwrap();
    retry.release().await;
    receiving.release().await;

    drop(held);
    LifecycleLock::acquire(url, &prod)
        .await
        .unwrap()
        .release()
        .await;
}

#[tokio::test]
async fn abandoned_sessions_expire_even_when_a_proxy_keeps_the_socket_open() {
    let database = wamn_test_postgres::database();
    let triple = Triple::new("acme", "wms", "prod");
    let mut tasks = JoinSet::new();
    let (observer, connection) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    tasks.spawn(async move {
        connection.await.unwrap();
    });

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
        let pid: i32 = held
            .client()
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .unwrap()
            .get(0);
        drop(held);
        timeout(Duration::from_secs(1), client_stopped)
            .await
            .unwrap()
            .unwrap();
        assert!(observer.query_one(
            "SELECT EXISTS (SELECT FROM pg_locks WHERE pid = $1 AND locktype = 'advisory' AND granted)",
            &[&pid],
        ).await.unwrap().get::<_, bool>(0), "the abandoned server session still holds its lock");

        let recovered = timeout(
            Duration::from_secs(7),
            acquire(database.url(), &triple, application),
        )
        .await
        .expect("an abandoned lock becomes available within the acquisition bound");
        recovered.release().await;
        assert!(
            !observer
                .query_one(
                    "SELECT EXISTS (SELECT FROM pg_stat_activity WHERE pid = $1)",
                    &[&pid]
                )
                .await
                .unwrap()
                .get::<_, bool>(0),
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
