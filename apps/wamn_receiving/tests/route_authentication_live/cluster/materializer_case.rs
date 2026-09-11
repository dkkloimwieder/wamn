//! The Receiving HTTP commands that cause the materializer test event.

use std::fs;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_ctl::dev::environment::{ProvisionedRoute, secret_value};
use wamn_gate_harness::journey::MaterializerPhase;

use super::{ReceivingCluster, apply, checked, kubectl, resources};

pub(super) async fn endpoint(cluster: &ReceivingCluster, name: &str) -> anyhow::Result<String> {
    let slices: Value = serde_json::from_slice(
        &checked(kubectl(&cluster.resources).args([
            "-n",
            &cluster.resources.name,
            "get",
            "endpointslices",
            "-l",
            "kubernetes.io/service-name=flow-http",
            "-o",
            "json",
        ]))
        .await?,
    )?;
    let service: Value = serde_json::from_slice(
        &checked(kubectl(&cluster.resources).args([
            "-n",
            &cluster.resources.name,
            "get",
            "service",
            "flow-http",
            "-o",
            "json",
        ]))
        .await?,
    )?;
    let slice = slices["items"]
        .as_array()
        .and_then(|items| items.first())
        .context("the released HTTP service has an EndpointSlice")?;
    let endpoints = slice["endpoints"].as_array().context("the HTTP slice has endpoints")?
        .iter().map(|endpoint| json!({"addresses":endpoint["addresses"],"conditions":endpoint["conditions"]}))
        .collect::<Vec<_>>();
    let objects = json!({"apiVersion":"v1","kind":"List","items":[
        {"apiVersion":"v1","kind":"Service","metadata":{"name":name,"namespace":cluster.resources.name},
         "spec":{"type":"NodePort","ports":[{"name":"http","protocol":"TCP",
            "port":service["spec"]["ports"][0]["port"],"targetPort":slice["ports"][0]["port"]}]}},
        {"apiVersion":"discovery.k8s.io/v1","kind":"EndpointSlice",
         "metadata":{"name":name,"namespace":cluster.resources.name,"labels":{"kubernetes.io/service-name":name}},
         "addressType":slice["addressType"],"ports":slice["ports"],"endpoints":endpoints},
    ]});
    let path = cluster.resources.work.join(format!("{name}.json"));
    fs::write(&path, serde_json::to_vec(&objects)?)?;
    apply(&cluster.resources, &path).await?;
    let observed: Value = serde_json::from_slice(
        &checked(kubectl(&cluster.resources).args([
            "-n",
            &cluster.resources.name,
            "get",
            "service",
            name,
            "-o",
            "json",
        ]))
        .await?,
    )?;
    let port = observed["spec"]["ports"][0]["nodePort"]
        .as_u64()
        .context("the owned HTTP service has an allocated NodePort")?;
    ensure!(
        (1000..=65535).contains(&port),
        "the owned HTTP NodePort is valid"
    );
    let address =
        resources::kind_address(&resources::inspect(&cluster.resources, "control-plane").await?)?;
    fs::write(
        cluster.resources.evidence.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&objects)?,
    )?;
    Ok(format!("http://{address}:{port}"))
}

pub(super) async fn trigger(
    cluster: &ReceivingCluster,
    route: &ProvisionedRoute,
    endpoint: &str,
) -> anyhow::Result<(MaterializerPhase, String, String)> {
    let update_trace = uuid::Uuid::new_v4().simple().to_string();
    let receipt_trace = uuid::Uuid::new_v4().simple().to_string();
    fs::write(
        cluster
            .resources
            .evidence
            .join("materializer-trace-ids.json"),
        serde_json::to_vec(&json!({"update":update_trace,"receipt":receipt_trace}))?,
    )?;
    let token = secret_value(&cluster.inputs.route_caller_secret_output, "token")?;
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(60))
        .build()?;
    tokio::time::timeout(Duration::from_secs(90), async {
        let update = http.post(format!("{endpoint}/acme/purchase_order/update"))
            .header("Host", &cluster.inputs.route_host).bearer_auth(&token)
            .header("traceparent", format!("00-{update_trace}-1111111111111111-01"))
            .json(&json!([{"request_id":"materializer-order","id":"00000000-0000-0000-0000-000000000304",
                "expected_row_version":"1","change":{"acme_inspection_required":true,"acme_quality_status":"pending"}}]))
            .send().await?;
        ensure!(update.status() == reqwest::StatusCode::OK, "the materializer order update must return HTTP 200");
        let update: Value = update.json().await?;
        fs::write(cluster.resources.evidence.join("materializer-update.json"), serde_json::to_vec_pretty(&update)?)?;
        ensure!(update.as_array().is_some_and(|items| items.len() == 1)
            && update[0]["request_id"] == "materializer-order"
            && update[0]["value"]["id"] == "00000000-0000-0000-0000-000000000304"
            && update[0]["value"]["row_version"] == "2"
            && update[0]["value"]["acme_inspection_required"] == true
            && update[0]["value"]["acme_quality_status"] == "pending",
            "the materializer order update must retain its expected state");
        let receipt = http.post(format!("{endpoint}/acme/receiving/record_receipt"))
            .header("Host", &cluster.inputs.route_host).bearer_auth(&token)
            .header("traceparent", format!("00-{receipt_trace}-2222222222222222-01"))
            .json(&json!([{"request_id":"materializer-receipt","value":{
                "idempotency_key":"materializer-receipt-command","purchase_order_id":"00000000-0000-0000-0000-000000000304",
                "receipt_reference":"MATERIALIZER-RECEIPT","occurred_at":"2026-08-31T12:34:00.000000Z",
                "line":[{"purchase_order_line_id":"00000000-0000-0000-0000-000000000504","quantity":"9.0000",
                    "location_id":"00000000-0000-0000-0000-000000000201"}]}}]))
            .send().await?;
        ensure!(receipt.status() == reqwest::StatusCode::OK, "the materializer Receipt command must return HTTP 200");
        let receipt: Value = receipt.json().await?;
        fs::write(cluster.resources.evidence.join("materializer-receipt.json"), serde_json::to_vec_pretty(&receipt)?)?;
        let receipt_id = receipt[0]["value"]["receipt_id"].as_str().context("the Receipt command returned its identity")?;
        ensure!(receipt.as_array().is_some_and(|items| items.len() == 1)
            && receipt[0]["request_id"] == "materializer-receipt"
            && receipt[0]["value"]["purchase_order_id"] == "00000000-0000-0000-0000-000000000304"
            && receipt[0]["value"]["purchase_order_status"] == "complete"
            && receipt[0]["value"]["row_version"] == "3"
            && receipt[0]["value"]["acme_inspection_required"] == true
            && receipt[0]["value"]["acme_quality_status"] == "pending"
            && receipt_id.len() == 36 && receipt_id.bytes().all(|byte| byte == b'-'
                || (byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())),
            "the materializer Receipt command must retain its expected state");
        Ok::<_, anyhow::Error>(MaterializerPhase {
            project_pg_url: route.database_url.clone(), nats_url: cluster.nats_url.clone(), receipt_id: receipt_id.to_owned(),
        })
    }).await.context("the two materializer HTTP commands exceeded 90 seconds")?
        .map(|phase| (phase, update_trace, receipt_trace))
}
