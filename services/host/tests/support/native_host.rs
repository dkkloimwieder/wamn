//! Observe native identity from the public heartbeat stream.

use anyhow::Context as _;
use futures_util::StreamExt as _;

pub async fn discover(client: &async_nats::Client, name: &str) -> anyhow::Result<String> {
    let mut heartbeats = client
        .subscribe(wash_runtime::washlet::heartbeat_subject("*"))
        .await?;
    client.flush().await?;
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while let Some(message) = heartbeats.next().await {
            let heartbeat: wash_runtime::washlet::types::v2::HostHeartbeat =
                serde_json::from_slice(&message.payload)?;
            if heartbeat.friendly_name == name {
                return Ok(heartbeat.id);
            }
        }
        anyhow::bail!("native heartbeat subscription closed")
    })
    .await
    .context("native identity was not published within the startup budget")?
}

pub fn write_settings(
    directory: &std::path::Path,
    org: &str,
    nats_url: &str,
) -> anyhow::Result<std::path::PathBuf> {
    let path = directory.join("settings.json");
    let settings = serde_json::json!({
        "org": org,
        "env": "test",
        "drain_bound_seconds": 300,
        "nats_url": nats_url,
        "insecure_registry": false,
        "projects": {},
    });
    std::fs::write(&path, serde_json::to_vec(&settings)?)?;
    std::fs::write(directory.join("host.yaml"), "host: {}\n")?;
    Ok(path)
}
