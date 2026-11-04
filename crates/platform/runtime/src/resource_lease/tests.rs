use super::*;

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

fn waiting(owner: &ResourceLeases, connection: uuid::Uuid) -> bool {
    owner
        .state
        .lock()
        .unwrap()
        .leases
        .get(&connection)
        .is_some_and(|lease| lease.state.load(Ordering::SeqCst) == PENDING)
}

#[tokio::test]
async fn local_owners_wait_and_release_after_the_call_finishes() {
    let a = Arc::new(ResourceLeases::new(None).unwrap());
    let b = Arc::new(ResourceLeases::new(None).unwrap());
    let connection = uuid::Uuid::new_v4();
    let target = uuid::Uuid::new_v4();
    let first = a.call(connection, target, "device", true).await.unwrap();
    let other = Arc::clone(&b);
    let second = tokio::spawn(async move { other.call(connection, target, "device", true).await });
    until(|| waiting(&b, connection)).await;
    assert!(!second.is_finished());
    a.invalidate(connection);
    tokio::task::yield_now().await;
    assert!(first.held());
    assert!(!second.is_finished());
    drop(first);
    let second = second.await.unwrap().unwrap();
    assert!(second.held());
    drop(second);
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn local_contention_expires_after_thirty_seconds() {
    let a = ResourceLeases::new(None).unwrap();
    let b = ResourceLeases::new(None).unwrap();
    let connection = uuid::Uuid::new_v4();
    let target = uuid::Uuid::new_v4();
    drop(a.call(connection, target, "device", true).await.unwrap());
    let started = tokio::time::Instant::now();
    assert!(b.call(connection, target, "device", true).await.is_err());
    assert_eq!(started.elapsed(), Duration::from_secs(30));
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn dropping_a_local_owner_releases_its_resource() {
    let a = ResourceLeases::new(None).unwrap();
    let b = Arc::new(ResourceLeases::new(None).unwrap());
    let connection = uuid::Uuid::new_v4();
    let target = uuid::Uuid::new_v4();
    drop(a.call(connection, target, "device", true).await.unwrap());
    let other = Arc::clone(&b);
    let waiting = tokio::spawn(async move { other.call(connection, target, "device", true).await });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    drop(a);
    drop(
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    );
    b.shutdown().await;
}

#[tokio::test]
async fn components_on_one_connection_do_not_overlap_and_shutdown_drains() {
    let owner = Arc::new(ResourceLeases::new(None).unwrap());
    let connection = uuid::Uuid::new_v4();
    let target = uuid::Uuid::new_v4();
    let reader = owner
        .call(connection, target, "device", true)
        .await
        .unwrap();
    let other = Arc::clone(&owner);
    let writer = tokio::spawn(async move { other.call(connection, target, "device", true).await });
    tokio::task::yield_now().await;
    assert!(!writer.is_finished());
    let other = Arc::clone(&owner);
    let shutdown = tokio::spawn(async move { other.shutdown().await });
    tokio::task::yield_now().await;
    assert!(!shutdown.is_finished());
    drop(reader);
    assert!(writer.await.unwrap().is_err());
    shutdown.await.unwrap();
    assert!(
        owner
            .call(connection, target, "device", true)
            .await
            .is_err()
    );
}

async fn database() -> (wamn_test_postgres::Database, tokio_postgres::Client) {
    let database = wamn_control_provision::test_database::system();
    let (client, driver) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(driver);
    (database, client)
}

async fn lock_pid(client: &tokio_postgres::Client, key: &str, granted: bool) -> i32 {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let row = client.query_opt(
                "SELECT pid FROM pg_locks WHERE locktype='advisory' AND granted=$2 AND classid=((hashtextextended($1,0)>>32)&4294967295)::oid AND objid=(hashtextextended($1,0)&4294967295)::oid AND objsubid=1",
                &[&key, &granted],
            ).await.unwrap();
            if let Some(row) = row { return row.get(0); }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap()
}

#[tokio::test]
async fn database_owners_unlock_and_recover_after_session_termination() {
    let _lock = wamn_test_postgres::lock();
    let (database, client) = database().await;
    let a = Arc::new(ResourceLeases::new(Some(database.url())).unwrap());
    let b = Arc::new(ResourceLeases::new(Some(database.url())).unwrap());
    let connection = uuid::Uuid::new_v4();
    let target = uuid::Uuid::new_v4();
    let key = format!("resource:{target}:device");
    drop(a.call(connection, target, "device", false).await.unwrap());
    let first_pid = lock_pid(&client, &key, true).await;
    let other = Arc::clone(&b);
    let second = tokio::spawn(async move { other.call(connection, target, "device", false).await });
    lock_pid(&client, &key, false).await;
    assert!(!second.is_finished());
    a.invalidate(connection);
    drop(second.await.unwrap().unwrap());
    b.invalidate(connection);
    drop(a.call(connection, target, "device", false).await.unwrap());
    assert_eq!(lock_pid(&client, &key, true).await, first_pid);
    let other = Arc::clone(&b);
    let second = tokio::spawn(async move { other.call(connection, target, "device", false).await });
    lock_pid(&client, &key, false).await;
    assert!(
        client
            .query_one("SELECT pg_terminate_backend($1)", &[&first_pid])
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    drop(second.await.unwrap().unwrap());
    until(|| {
        a.state.lock().unwrap().leases[&connection]
            .state
            .load(Ordering::SeqCst)
            == LOST
    })
    .await;
    assert!(a.call(connection, target, "device", false).await.is_err());
    b.shutdown().await;
    until(|| {
        a.state.lock().unwrap().leases[&connection]
            .state
            .load(Ordering::SeqCst)
            == HELD
    })
    .await;
    drop(a.call(connection, target, "device", false).await.unwrap());
    assert_ne!(lock_pid(&client, &key, true).await, first_pid);
    a.shutdown().await;
}

#[tokio::test]
async fn a_waiting_acquire_does_not_block_another_resource_release() {
    let _lock = wamn_test_postgres::lock();
    let (database, client) = database().await;
    let owner = Arc::new(ResourceLeases::new(Some(database.url())).unwrap());
    let target = uuid::Uuid::new_v4();
    let first = uuid::Uuid::new_v4();
    let second = uuid::Uuid::new_v4();
    let blocked = format!("resource:{target}:blocked");
    client
        .query_one(
            "SELECT pg_advisory_lock(hashtextextended($1,0))",
            &[&blocked],
        )
        .await
        .unwrap();
    drop(owner.call(first, target, "free", false).await.unwrap());
    let other = Arc::clone(&owner);
    let pending = tokio::spawn(async move { other.call(second, target, "blocked", false).await });
    lock_pid(&client, &blocked, false).await;
    drop(
        tokio::time::timeout(
            Duration::from_secs(5),
            owner.call(uuid::Uuid::new_v4(), target, "another", false),
        )
        .await
        .unwrap()
        .unwrap(),
    );
    owner.invalidate(first);
    until(|| {
        owner.state.lock().unwrap().leases[&first]
            .state
            .load(Ordering::SeqCst)
            == RELEASED
    })
    .await;
    client
        .query_one(
            "SELECT pg_advisory_unlock(hashtextextended($1,0))",
            &[&blocked],
        )
        .await
        .unwrap();
    drop(pending.await.unwrap().unwrap());
    owner.shutdown().await;
}

#[tokio::test]
async fn a_replacement_on_the_same_owner_waits_for_the_old_call() {
    let _lock = wamn_test_postgres::lock();
    let (database, _) = database().await;
    let owner = Arc::new(ResourceLeases::new(Some(database.url())).unwrap());
    let target = uuid::Uuid::new_v4();
    let old = uuid::Uuid::new_v4();
    let replacement = uuid::Uuid::new_v4();
    let call = owner.call(old, target, "device", false).await.unwrap();
    owner.invalidate(old);
    let other = Arc::clone(&owner);
    let next = tokio::spawn(async move { other.call(replacement, target, "device", false).await });
    until(|| waiting(&owner, replacement)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!next.is_finished());
    drop(call);
    drop(next.await.unwrap().unwrap());
    owner.shutdown().await;
}
