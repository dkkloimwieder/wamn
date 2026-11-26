//! Live test of the lifecycle lock
//! (docs/architecture/overview.md#environment-lifecycle, wamn-snz0.1): a second
//! verb on the same coordinate refuses with "deployment in progress",
//! another coordinate is not blocked, and closing the holder's connection
//! releases the lock.

use sqlx::Row as _;
use std::time::Duration;
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time::timeout,
};
use wamn_env::registry::lifecycle_lock::LifecycleLock;
use wamn_registry::Triple;

#[tokio::test]
#[ignore = "requires: docker"]
async fn a_second_verb_refuses_until_the_holder_is_gone() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = postgres.create_database("project").await.unwrap();
    let url = database.as_str();
    let prod = Triple::new(
        "acme".parse().unwrap(),
        "prod".parse().unwrap(),
        "wms".parse().unwrap(),
    );

    LifecycleLock::hold(
        wamn_postgres::connect(url).await.unwrap(),
        &prod,
        async |_| {
            assert!(
                timeout(
                    Duration::from_secs(7),
                    LifecycleLock::acquire(wamn_postgres::connect(url).await.unwrap(), &prod)
                )
                .await?
                .is_err()
            );
            assert!(
                LifecycleLock::acquire_app(
                    wamn_postgres::connect(url).await.unwrap(),
                    &prod,
                    &"orders".parse().expect("valid name")
                )
                .await
                .is_err()
            );
            let other = LifecycleLock::acquire(
                wamn_postgres::connect(url).await.unwrap(),
                &Triple::new(
                    "acme".parse().unwrap(),
                    "dev".parse().unwrap(),
                    "wms".parse().unwrap(),
                ),
            )
            .await?;
            other.release().await;
            Ok(())
        },
    )
    .await
    .unwrap();
    let orders = LifecycleLock::acquire_app(
        wamn_postgres::connect(url).await.unwrap(),
        &prod,
        &"orders".parse().expect("valid name"),
    )
    .await
    .unwrap();
    assert!(
        timeout(
            Duration::from_secs(7),
            LifecycleLock::acquire_app(
                wamn_postgres::connect(url).await.unwrap(),
                &prod,
                &"orders".parse().expect("valid name")
            ),
        )
        .await
        .unwrap()
        .is_err()
    );
    let receiving = LifecycleLock::acquire_app(
        wamn_postgres::connect(url).await.unwrap(),
        &prod,
        &"receiving".parse().expect("valid name"),
    )
    .await
    .unwrap();
    assert!(
        LifecycleLock::acquire(wamn_postgres::connect(url).await.unwrap(), &prod)
            .await
            .is_err()
    );
    orders.release().await;
    let retry = LifecycleLock::acquire_app(
        wamn_postgres::connect(url).await.unwrap(),
        &prod,
        &"orders".parse().expect("valid name"),
    )
    .await
    .unwrap();
    retry.release().await;
    receiving.release().await;

    // A SQL Job keeps its own shared session after the CLI session disappears.
    let cli = LifecycleLock::acquire_shared(wamn_postgres::connect(url).await.unwrap(), &prod)
        .await
        .unwrap();
    let job = LifecycleLock::acquire_shared(wamn_postgres::connect(url).await.unwrap(), &prod)
        .await
        .unwrap();
    drop(cli);
    assert!(
        LifecycleLock::acquire(wamn_postgres::connect(url).await.unwrap(), &prod)
            .await
            .is_err()
    );
    let mut waiting_url = url::Url::parse(url).unwrap();
    waiting_url
        .query_pairs_mut()
        .append_pair("application_name", "lifecycle_wait");
    let mut observer = wamn_postgres::connect(url).await.unwrap();
    let waiting = LifecycleLock::acquire_wait(
        wamn_postgres::connect(waiting_url.as_str()).await.unwrap(),
        &prod,
    );
    // The job releases only after the waiter has tried the held lock.
    let release = async {
        timeout(Duration::from_secs(5), async {
            while !sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (SELECT FROM pg_stat_activity \
                 WHERE application_name = 'lifecycle_wait' AND query = $1)",
            )
            .bind(wamn_postgres::ENV_LOCK_SQL)
            .fetch_one(&mut observer)
            .await
            .unwrap()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the waiter tries the held lock");
        job.release().await;
    };
    let (waited, ()) = timeout(Duration::from_secs(10), async {
        tokio::join!(waiting, release)
    })
    .await
    .unwrap();
    waited.unwrap().release().await;
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn abandoned_sessions_expire_even_when_a_proxy_keeps_the_socket_open() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = postgres.create_database("project").await.unwrap();
    let triple = Triple::new(
        "acme".parse().unwrap(),
        "prod".parse().unwrap(),
        "wms".parse().unwrap(),
    );
    let mut tasks = JoinSet::new();
    let mut observer = wamn_postgres::connect(database.as_str()).await.unwrap();
    tasks.spawn(async move {});

    let orders = "orders".parse().unwrap();
    for app in [None, Some(&orders)] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut proxy_url = url::Url::parse(database.as_str()).unwrap();
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
        let held = acquire(proxy_url.as_str(), &triple, app).await;
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
        assert!(sqlx::query("SELECT EXISTS (SELECT FROM pg_locks WHERE pid = $1 AND locktype = 'advisory' AND granted)").bind(pid).fetch_one(&mut observer).await.unwrap().get::<bool, _>(0), "the abandoned server session still holds its lock");

        let recovered = timeout(
            Duration::from_secs(7),
            acquire(database.as_str(), &triple, app),
        )
        .await
        .expect("an abandoned lock becomes available within the acquisition bound");
        recovered.release().await;
        assert!(
            !sqlx::query("SELECT EXISTS (SELECT FROM pg_stat_activity WHERE pid = $1)")
                .bind(pid)
                .fetch_one(&mut observer)
                .await
                .unwrap()
                .get::<bool, _>(0),
            "PostgreSQL ends the abandoned session"
        );
    }
}

async fn acquire(
    url: &str,
    triple: &Triple,
    app: Option<&wamn_registry::AppName>,
) -> LifecycleLock {
    match app {
        Some(app) => {
            LifecycleLock::acquire_app(wamn_postgres::connect(url).await.unwrap(), triple, app)
                .await
        }
        None => LifecycleLock::acquire(wamn_postgres::connect(url).await.unwrap(), triple).await,
    }
    .unwrap()
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn lock_loss_cancels_the_protected_operation() -> anyhow::Result<()> {
    let postgres = wamn_test_infrastructure::containers::start_postgres().await?;
    let database = postgres.create_database("project").await?;
    let triple = Triple::new(
        "acme".parse().unwrap(),
        "prod".parse().unwrap(),
        "wms".parse().unwrap(),
    );
    let mut observer = wamn_postgres::connect(database.as_str()).await?;
    let lock =
        LifecycleLock::acquire(wamn_postgres::connect(database.as_str()).await?, &triple).await?;
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
    LifecycleLock::acquire(wamn_postgres::connect(database.as_str()).await?, &triple)
        .await?
        .release()
        .await;
    Ok(())
}
