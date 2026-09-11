//! The optional browser route remains reachable until the owned test is released.

use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::ensure;
use serde_json::json;
use tokio::process::Command;

use super::deployment::checked;
use crate::wms_runtime_live::write_result;

pub(super) async fn start(
    lifecycle: &Path,
    cluster: &str,
    work: &Path,
    endpoint: &str,
    route_host: &str,
    evidence: &Path,
) -> anyhow::Result<()> {
    let proxy = format!(
        "server {{\n  listen 80;\n  location / {{\n    proxy_pass {endpoint};\n    proxy_set_header Host {route_host};\n    proxy_http_version 1.1;\n  }}\n}}\n"
    );
    fs::write(work.join("demo-proxy.conf"), proxy)?;
    checked(
        Command::new(lifecycle)
            .arg("demo-proxy")
            .arg(cluster)
            .arg(work),
    )
    .await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    for _ in 0..30 {
        if let Ok(response) = client.get("http://127.0.0.1:8080/").send().await {
            write_result(
                evidence,
                "demo.json",
                &json!({"url":"http://127.0.0.1:8080/","status":response.status().as_u16(),"route_host":route_host,"node_port":30950}),
            )?;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    anyhow::bail!("the demo proxy answered no HTTP status after 30 attempts")
}

pub(super) async fn hold(work: &Path, enabled: bool) -> anyhow::Result<()> {
    let seconds = match std::env::var("WAMN_JOURNEY_HOLD_SECONDS") {
        Ok(value) => value.parse::<f64>()?,
        Err(std::env::VarError::NotPresent) => {
            if enabled {
                3600.0
            } else {
                0.0
            }
        }
        Err(error) => return Err(error.into()),
    };
    ensure!(
        seconds.is_finite() && seconds >= 0.0,
        "the test hold must be a nonnegative number of seconds"
    );
    let duration = Duration::try_from_secs_f64(seconds)?;
    if enabled {
        eprintln!(
            "WMS browser route: http://127.0.0.1:8080/. The private caller token is in {}. Use /pallet/get to read pallet {} before sending /inventory/move with its current row_version. The environment remains available for {seconds} seconds or until interrupted.",
            work.join("route-caller-pat.json").display(),
            super::application::PALLET_ID
        );
    }
    tokio::time::sleep(duration).await;
    Ok(())
}
