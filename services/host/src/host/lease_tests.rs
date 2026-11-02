//! Live lease handoff through the host's SIGTERM and queue drain owner.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use wamn_runtime::auth_cache::test_util::{nats, until};
use wamn_runtime::connection_memory::{ConnectionFact, ConnectionStatus};
use wamn_runtime::plugins::wamn_postgres::WamnPostgresConfig;
use wamn_runtime::resource_lease::ResourceLeases;

async fn facts(
    client: &async_nats::Client,
    url: &str,
    fact: &ConnectionFact,
) -> anyhow::Result<(Arc<ConnectionFacts>, Arc<ResourceLeases>)> {
    let leases = Arc::new(ResourceLeases::new(Some(url))?);
    let memory = Arc::new(ConnectionMemory::default().with_resource_leases(Arc::clone(&leases)));
    let changes =
        connection_memory::replicate(client, Some("lease--rollout--dev"), Arc::clone(&memory));
    until("the rollout host watches connection facts", || {
        memory.watched()
    })
    .await;
    let id = "00000000-0000-4000-8000-000000000001".parse()?;
    if let Err(ticket) = memory.connection("lease--rollout--dev", &id) {
        memory
            .keep_connection("lease--rollout--dev", &id, ticket, fact.clone())
            .await;
    }
    until("the rollout host holds the connection fact", || {
        memory.connection("lease--rollout--dev", &id).is_ok()
    })
    .await;
    let facts = ConnectionFacts::new(
        memory,
        changes,
        Arc::new(ConnectionCredentials::empty()),
        Arc::new(WamnPostgres::new(WamnPostgresConfig {
            credentials: None,
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 5_000,
            statement_timeout_ms: 10_000,
            row_limit: 10_000,
        })?),
        "lease--rollout--dev",
        "rollout",
        "dev",
    );
    Ok((Arc::new(facts), leases))
}

async fn ready(address: SocketAddr) -> anyhow::Result<()> {
    let mut stream = tokio::net::TcpStream::connect(address).await?;
    stream
        .write_all(b"GET /readyz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    anyhow::ensure!(
        response.starts_with("HTTP/1.1 200") && response.ends_with("ok\n"),
        "readiness refused during lease contention: {response}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_NATIVE_C_NATS_BIN"]
async fn sigterm_drains_calls_then_hands_the_lease_to_a_ready_host() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_NATIVE_C_NATS_BIN",
    );
    let database = wamn_test_postgres::database();
    let (admin, driver) = tokio_postgres::connect(database.url(), NoTls).await?;
    let driver = tokio::spawn(driver);
    let (_nats, client) = nats().await;
    let connection = "00000000-0000-4000-8000-000000000001".parse()?;
    let target = "00000000-0000-4000-8000-000000000002".parse()?;
    let fact = ConnectionFact {
        active_generation: 1,
        status: ConnectionStatus::Active,
        credential_id: None,
        service_principal_id: "00000000-0000-4000-8000-000000000003".parse()?,
        exclusive: true,
        target_id: target,
        resource_key: Some("tcp:device:502".into()),
        target_type: "cloud".into(),
    };
    let (a, leases_a) = facts(&client, database.url(), &fact).await?;
    let call_a = a
        .exclusive_call(connection)
        .await?
        .context("host A needs an exclusive call")?;
    let key = format!("resource:{target}:tcp:device:502");
    let pid: i32 = admin.query_one("SELECT pid FROM pg_locks WHERE locktype='advisory' AND granted AND classid=((hashtextextended($1,0)>>32)&4294967295)::oid AND objid=(hashtextextended($1,0)&4294967295)::oid AND objsubid=1", &[&key]).await?.get(0);

    let (b, leases_b) = facts(&client, database.url(), &fact).await?;
    let probes_b = ProbeState::default();
    let listener = probes::bind("127.0.0.1:0".parse()?).await?;
    let address = listener.local_addr()?;
    let (stop_probe, probe_stopped) = tokio::sync::oneshot::channel();
    let probe = tokio::spawn(probes::serve(listener, probes_b.clone(), async {
        let _ = probe_stopped.await;
    }));
    probes_b.started();
    let pending = tokio::spawn(async move { b.exclusive_call(connection).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = admin.query_one("SELECT EXISTS(SELECT FROM pg_locks WHERE locktype='advisory' AND NOT granted AND classid=((hashtextextended($1,0)>>32)&4294967295)::oid AND objid=(hashtextextended($1,0)&4294967295)::oid AND objsubid=1)", &[&key]).await?.get(0);
            if waiting { return Ok::<_, anyhow::Error>(()); }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await??;
    ready(address).await?;
    assert!(!pending.is_finished());

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let (stop_queue, mut stopping) = tokio::sync::watch::channel(false);
    let (draining, draining_started) = tokio::sync::oneshot::channel();
    let (finish, finished) = tokio::sync::oneshot::channel();
    let native_stopped = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&native_stopped);
    let shutdown = tokio::spawn(async move {
        let queue = async {
            stopping.changed().await?;
            let _ = draining.send(());
            finished.await?;
            assert!(call_a.held());
            drop(call_a);
            Ok(())
        };
        tokio::pin!(queue);
        stop_combined_after(
            async { sigterm.recv().await.context("receive test SIGTERM") },
            async { Ok(()) },
            false,
            async {
                leases_a.shutdown().await;
                stopped.store(true, Ordering::SeqCst);
                Ok(())
            },
            queue.as_mut(),
            true,
            stop_queue,
            &ProbeState::default(),
            Duration::ZERO,
            Duration::from_secs(60),
        )
        .await
    });
    let status = tokio::process::Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .await?;
    anyhow::ensure!(status.success(), "send SIGTERM to this test process");
    tokio::time::timeout(Duration::from_secs(5), draining_started).await??;
    assert!(!native_stopped.load(Ordering::SeqCst));
    assert!(!pending.is_finished());
    ready(address).await?;
    finish.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(60), shutdown).await???;
    let call_b = tokio::time::timeout(Duration::from_secs(5), pending)
        .await???
        .context("host B needs an exclusive call")?;
    assert!(call_b.held());
    assert!(native_stopped.load(Ordering::SeqCst));
    ready(address).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !admin
                .query_one(
                    "SELECT EXISTS(SELECT FROM pg_stat_activity WHERE pid=$1)",
                    &[&pid],
                )
                .await?
                .get::<_, bool>(0)
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    drop(call_b);
    leases_b.shutdown().await;
    let _ = stop_probe.send(());
    probe.await?;
    drop(admin);
    driver.await??;
    Ok(())
}
