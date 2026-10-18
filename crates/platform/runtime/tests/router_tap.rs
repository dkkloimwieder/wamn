//! Requires WAMN_NATIVE_C_NATS_BIN, outside the lib/bins unit test run.

use std::borrow::Cow;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use async_nats::jetstream::stream::{Config, StorageType};
use futures_util::{FutureExt as _, StreamExt as _};
use serde_json::json;
use tokio::time::timeout;
use wamn_catalog::AttachmentTarget;
use wamn_control_provision::events::{advisory_stream_config, source_stream_config};
use wamn_control_registry::Triple;
use wamn_runtime::plugins::wamn_jetstream::{
    RouterTapPhase, RouterTapPreview, RouterTapRecord, WamnJetstream, WamnJetstreamConfig,
    router_tap_environment_filter,
};

struct Broker {
    process: Child,
    directory: PathBuf,
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.process.kill().expect("stop test NATS");
        self.process.wait().expect("reap test NATS");
        std::fs::remove_dir_all(&self.directory).expect("remove test NATS storage");
    }
}

#[tokio::test]
async fn router_tap_never_waits_for_broker() -> anyhow::Result<()> {
    let binary = std::env::var_os("WAMN_NATIVE_C_NATS_BIN")
        .context("set WAMN_NATIVE_C_NATS_BIN to a local nats-server executable")?;
    let directory = std::env::temp_dir().join(format!(
        "wamn-router-tap-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
    ));
    std::fs::create_dir(&directory)?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let mut broker = Broker {
        process: Command::new(binary)
            .args(["-js", "-a", "127.0.0.1", "-p", &port.to_string(), "-sd"])
            .arg(&directory)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
        directory,
    };
    let url = format!("nats://127.0.0.1:{port}");
    let client = timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(client) = async_nats::connect(&url).await {
                break client;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    let context = async_nats::jetstream::new(client.clone());
    let scope = Triple::new("tap-test", "project", "test");
    for config in [
        source_stream_config(&scope, 1, Duration::from_secs(120)),
        advisory_stream_config(&scope, 1),
    ] {
        context.create_stream(config).await?;
    }
    let plugin = WamnJetstream::new(WamnJetstreamConfig {
        nats_url: Some(url),
        event_scope: Some(scope),
        stream_replicas: Some(1),
        dup_window_secs: Some(120),
        ..Default::default()
    });
    plugin
        .activate_events()
        .await
        .map_err(|error| anyhow::anyhow!("activate test event streams: {error:?}"))?;
    plugin.bind_derived_scope("component", "tap-test", "project", "test")?;
    let filter = router_tap_environment_filter("tap-test", "project", "test").unwrap();
    let mut previews = client.subscribe(filter.clone()).await?;
    client.flush().await?;
    let target = AttachmentTarget::Route {
        component: "component".into(),
        operation: "get".into(),
    };
    let payload = json!({"value": 7, "password": "secret"});
    let publish = || {
        plugin.publish_router_tap(
            "component",
            RouterTapPreview {
                delivery_id: "delivery",
                target: &target,
                source_type: "attachment",
                source_id: "get-route",
                phase: RouterTapPhase::Accepted,
                payload: Cow::Borrowed(&payload),
            },
        )
    };

    // Exceed async-nats 0.49.1's default 5,000 pending acknowledgements.
    assert!(context.get_stream("WAMN_TAP").await.is_err());
    timeout(Duration::from_secs(2), async {
        for _ in 0..6_000 {
            publish().await;
        }
    })
    .await
    .expect("router taps waited for acknowledgements without WAMN_TAP");
    let preview = timeout(Duration::from_secs(2), previews.next())
        .await?
        .context("tap subscription closed")?;
    let record: RouterTapRecord = serde_json::from_slice(&preview.payload)?;
    record.validate()?;
    assert_eq!(record.delivery_id.as_ref(), "delivery");
    assert_eq!(record.payload["value"], 7);
    assert!(record.redacted);
    assert_ne!(record.payload["password"], "secret");

    let stream = context
        .create_stream(Config {
            name: "WAMN_TAP".into(),
            subjects: vec![filter],
            storage: StorageType::Memory,
            ..Default::default()
        })
        .await?;
    publish().await;
    timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(message) = stream.get_raw_message(1).await {
                assert_eq!(message.payload, preview.payload);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("WAMN_TAP did not capture the core NATS preview")?;

    // Fill the client send queue after the broker exits.
    broker.process.kill()?;
    broker.process.wait()?;
    timeout(Duration::from_secs(2), async {
        for _ in 0..6_000 {
            assert!(
                publish().now_or_never().is_some(),
                "router tap yielded to an unavailable broker"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("router taps waited for an unavailable broker");
    Ok(())
}
