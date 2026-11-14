//! Routes retain service while the scheduler is unavailable and after recovery.
use anyhow::{Context as _, ensure};
use futures_util::StreamExt as _;
use std::time::Duration;
use tokio::time::{Instant, sleep, timeout};
use wamn_test_application::Application;
const OUTAGE: Duration = Duration::from_secs(30);
const RECOVERY_BUDGET: Duration = Duration::from_secs(60);

#[tokio::test]
async fn routes_answer_during_a_scheduler_outage_and_recover() -> anyhow::Result<()> {
    let application: Application = panic!("wamn-eulfr.12.10: no product scheduler outage fixture");
    let host_name: String = panic!("wamn-eulfr.12.10: no product heartbeat observer");
    let route = Route {
        http: reqwest::Client::new(),
        url: format!("{}/location/list", application.routes.endpoint),
        host: application.routes.host.clone(),
        token: application.routes.bearer.clone(),
    };
    route.answers("before the outage").await?;

    panic!("wamn-eulfr.12.10: no product scheduler stop");
    let stopped = Instant::now();
    let mut answered = 0;
    let mut slowest = Duration::ZERO;
    while stopped.elapsed() < OUTAGE {
        let sent = Instant::now();
        route.answers("during the scheduler outage").await?;
        slowest = slowest.max(sent.elapsed());
        answered += 1;
        sleep(Duration::from_millis(500)).await;
    }

    panic!("wamn-eulfr.12.10: no product scheduler restart");
    let restarted = Instant::now();
    let operator: async_nats::Client = panic!("wamn-eulfr.12.10: no product heartbeat observer");
    let mut heartbeats = operator.subscribe("runtime.operator.heartbeat.*").await?;
    let heartbeat = timeout(RECOVERY_BUDGET, heartbeats.next())
        .await
        .context("the host published no heartbeat through the restarted scheduler NATS")?
        .context("the heartbeat subscription ended")?;
    let recovered_after = restarted.elapsed();
    let heartbeat = String::from_utf8(heartbeat.payload.to_vec())?;
    ensure!(
        heartbeat.contains(host_name.as_str()),
        "the heartbeat after the outage is not this host's: {heartbeat}"
    );
    route.answers("after the scheduler returned").await?;
    println!(
        "SCHEDULER_OUTAGE result=pass outage_seconds={} answered_during_outage={answered} slowest_answer_ms={} heartbeat_after_restart_ms={}",
        OUTAGE.as_secs(),
        slowest.as_millis(),
        recovered_after.as_millis()
    );
    application.remove().await?;
    Ok(())
}

/// The authenticated location read of the served route.
struct Route {
    http: reqwest::Client,
    url: String,
    host: String,
    token: String,
}

impl Route {
    async fn answers(&self, when: &str) -> anyhow::Result<()> {
        let response = self
            .http
            .get(&self.url)
            .header("Host", &self.host)
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("the route did not answer {when}"))?;
        ensure!(
            response.status() == reqwest::StatusCode::OK,
            "the route answered {} {when}",
            response.status()
        );
        let body: serde_json::Value = response.json().await?;
        ensure!(
            body[0]["value"]["rows"][0]["location_code"] == "DOCK-1",
            "the route answered the wrong rows {when}: {body}"
        );
        Ok(())
    }
}
