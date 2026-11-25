//! The short smoke path through product commands and web.

mod common;

use std::path::Path;

use anyhow::{Context as _, ensure};
use common::Org;
use serde_json::Value;
use wamn_http_client::HttpClient;

const APPLICATION: &str = "wamn_receiving";

/// An owner deploys one application, and web answers 401 without a token and 200 with
/// the owner token, for one application call and one control call.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: cluster, bin:helm"]
async fn the_owner_reaches_an_application_and_control_through_web() -> anyhow::Result<()> {
    common::capture(
        "the_owner_reaches_an_application_and_control_through_web",
        the_owner_reaches_an_application_and_control_through_web_case,
    )
    .await
}

async fn the_owner_reaches_an_application_and_control_through_web_case() -> anyhow::Result<()> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps")
        .join(APPLICATION);
    let environment = Org::create().await?.apply(APPLICATION).await?;
    environment.deploy(&source, APPLICATION).await?;

    let org = &environment.org;
    let outputs = &org.cluster.outputs;
    let file: Value = serde_json::from_slice(&std::fs::read(&environment.file)?)?;
    let label = file["projects"][0]["applications"][APPLICATION]["label"]
        .as_str()
        .context("the environment file has no application label")?;
    let application =
        wamn_registry::name::application_host(label, &org.name, &outputs.base_domain.parse()?);
    let control = format!("{}.{}", org.name, outputs.base_domain);
    let client = HttpClient::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    for (host, path) in [
        (application.as_str(), "/api/location/list"),
        (control.as_str(), "/wamn_control/control/mine"),
    ] {
        let url = format!("http://{}{path}", outputs.web_address);
        let refused = client.get(&url).header("Host", host).send().await?;
        let status = refused.status();
        ensure!(
            status == hyper::StatusCode::UNAUTHORIZED,
            "{host}{path} without a token answered {status}: {}",
            refused.text().await?
        );
        let answered = client
            .get(&url)
            .header("Host", host)
            .bearer_auth(&org.token)
            .send()
            .await?;
        let status = answered.status();
        let text = answered.text().await?;
        ensure!(
            status == hyper::StatusCode::OK,
            "{host}{path} with the owner token answered {status}: {text}"
        );
        serde_json::from_str::<Value>(&text)
            .with_context(|| format!("{host}{path} answered no JSON: {text}"))?;
    }
    Ok(())
}
