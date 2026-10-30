//! A private NATS server and cache-watch readiness for live tests.

use super::*;

/// A `nats-server -js` on a free port, stopped with the guard.
#[derive(Debug)]
pub struct Server(std::process::Child, std::path::PathBuf);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

/// A private JetStream NATS at `WAMN_NATIVE_C_NATS_BIN`, and a client of
/// it.
pub async fn nats() -> (Server, async_nats::Client) {
    let binary = std::env::var_os("WAMN_NATIVE_C_NATS_BIN")
        .expect("set WAMN_NATIVE_C_NATS_BIN to a nats-server executable");
    let store = std::env::temp_dir().join(format!("wamn-auth-kv-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&store).unwrap();
    // NATS binds the free port itself and publishes the bound address.
    let child = std::process::Command::new(&binary)
        .args(["-js", "-a", "127.0.0.1", "-p", "-1", "-sd"])
        .arg(&store)
        .arg("--ports_file_dir")
        .arg(&store)
        .spawn()
        .unwrap();
    let ports = store.join(format!(
        "{}_{}.ports",
        std::path::Path::new(&binary)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap(),
        child.id()
    ));
    let mut server = Server(child, store);
    let deadline = Instant::now() + Duration::from_secs(5);
    let url = loop {
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "NATS exited during startup"
        );
        if let Ok(bytes) = tokio::fs::read(&ports).await
            && let Ok(ports) = serde_json::from_slice::<serde_json::Value>(&bytes)
            && let Some(url) = ports["nats"][0].as_str()
        {
            break url.to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "NATS did not publish its client port"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    loop {
        match async_nats::connect(&url).await {
            Ok(client) => break (server, client),
            Err(error) => assert!(Instant::now() < deadline, "{error}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Replicate the memory of `holder` from the bucket on `client`, and
/// wait until the watch runs.
pub async fn watched<H: AsRef<AuthCache> + Send + Sync + 'static>(
    client: &async_nats::Client,
    env_key: Option<&str>,
    holder: Arc<H>,
) -> AuthChanges {
    let writer = replicate(client, env_key, Arc::clone(&holder));
    until("the watch runs", || {
        (*holder).as_ref().memory.bucket().is_some()
    })
    .await;
    writer
}

pub async fn until(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
