//! The Receiving HTTP commands that cause the materializer test event.

use std::fs;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_control::provision_project_env::secret_value;
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
    fs::write(
        cluster
            .resources
            .evidence
            .join(format!("{name}-service.json")),
        serde_json::to_vec_pretty(&observed)?,
    )?;
    let endpoint = format!("http://{address}:{port}");
    ready(cluster, name, &endpoint).await?;
    Ok(endpoint)
}

async fn ready(cluster: &ReceivingCluster, name: &str, endpoint: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()?;
    let mut attempts = Vec::new();
    // The retained NodePort caller probes this transport before sending mutations.
    // Service allocation alone does not establish that the node forwards traffic.
    let result = tokio::time::timeout(Duration::from_secs(45), async {
        for attempt in 0..=15 {
            let response = async {
                let response = client
                    .get(format!("{endpoint}/no-such-route"))
                    .header("Host", &cluster.inputs.route_host)
                    .send()
                    .await?;
                let status = response.status().as_u16();
                let content_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                let body = response.bytes().await?;
                Ok::<_, reqwest::Error>((status, content_type, body))
            }
            .await;
            match response {
                Ok((status, content_type, body)) => {
                    attempts.push(json!({"attempt":attempt,"status":status,
                        "content_type":content_type,"body_hex":hex::encode(&body)}));
                    if !matches!(status, 408 | 429 | 500 | 502 | 503 | 504) || attempt == 15 {
                        return validate_response(status, &content_type, &body);
                    }
                }
                Err(error) => {
                    let timed_out = error.is_timeout();
                    let error = anyhow::Error::new(error);
                    attempts.push(json!({"attempt":attempt,"failure":format!("{error:#}")}));
                    if attempt == 15
                        || !(timed_out
                            || wamn_integration_tests::operator_recovery::connection_refused(
                                &error,
                            ))
                    {
                        return Err(error);
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        unreachable!("the final probe attempt returns its result")
    })
    .await
    .context("the owned NodePort did not return its route response within 45 seconds")
    .and_then(|result| result);
    fs::write(
        cluster
            .resources
            .evidence
            .join(format!("{name}-readiness.json")),
        serde_json::to_vec_pretty(&json!({"origin":"host-nodeport","endpoint":endpoint,
            "host":cluster.inputs.route_host,"path":"/no-such-route","attempts":attempts,
            "passed":result.is_ok(),"failure":result.as_ref().err().map(|error| format!("{error:#}"))}))?,
    )?;
    result
}

fn validate_response(status: u16, content_type: &str, body: &[u8]) -> anyhow::Result<()> {
    ensure!(
        status == 404
            && content_type == "application/json"
            && body == br#"{"error":{"code":"route-not-found"}}"#,
        "the owned NodePort must return the released route's exact HTTP 404 response"
    );
    Ok(())
}

/// The untouched purchase order that one trigger updates and receives against.
pub(super) struct TriggerOrder {
    pub order_id: String,
    pub line_id: String,
    pub idempotency_key: String,
}

impl TriggerOrder {
    /// The order that provisioning seeds for the first trigger on a cluster.
    pub(super) fn seeded() -> Self {
        Self {
            order_id: "00000000-0000-0000-0000-000000000304".to_owned(),
            line_id: "00000000-0000-0000-0000-000000000504".to_owned(),
            idempotency_key: "materializer-receipt-command".to_owned(),
        }
    }

    /// Seed a new order, so a stage can trigger again on a kept cluster.
    pub(super) async fn fresh(database_url: &str) -> anyhow::Result<Self> {
        let run = uuid::Uuid::new_v4();
        let order = Self {
            order_id: run.to_string(),
            line_id: uuid::Uuid::new_v4().to_string(),
            idempotency_key: format!("materializer-receipt-command-{}", run.simple()),
        };
        let (project, task) = super::super::connect(database_url).await?;
        let seeded = super::super::seed_materializer_order(
            project.as_ref(),
            &order.order_id,
            &order.line_id,
            &format!("PO-{}", run.simple()),
        )
        .await;
        task.abort();
        seeded?;
        Ok(order)
    }
}

pub(super) async fn trigger(
    cluster: &ReceivingCluster,
    database_url: &str,
    endpoint: &str,
    order: &TriggerOrder,
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
            .json(&json!([{"request_id":"materializer-order","id":order.order_id,
                "expected_row_version":1,"change":{"acme_inspection_required":true,"acme_quality_status":"pending"}}]))
            .send().await?;
        ensure!(update.status() == reqwest::StatusCode::OK, "the materializer order update must return HTTP 200");
        let update: Value = update.json().await?;
        fs::write(cluster.resources.evidence.join("materializer-update.json"), serde_json::to_vec_pretty(&update)?)?;
        ensure!(update.as_array().is_some_and(|items| items.len() == 1)
            && update[0]["request_id"] == "materializer-order"
            && update[0]["value"]["id"] == order.order_id.as_str()
            && update[0]["value"]["row_version"] == 2
            && update[0]["value"]["acme_inspection_required"] == true
            && update[0]["value"]["acme_quality_status"] == "pending",
            "the materializer order update must retain its expected state");
        // This case observes post-commit creation of a pending inspection by the
        // materializer. The permitted base route does not require pre-commit QC.
        let receipt = http.post(format!("{endpoint}/receiving/record_receipt"))
            .header("Host", &cluster.inputs.route_host).bearer_auth(&token)
            .header("traceparent", format!("00-{receipt_trace}-2222222222222222-01"))
            .json(&json!([{"request_id":"materializer-receipt","value":{
                "idempotency_key":order.idempotency_key,"purchase_order_id":order.order_id,
                "receipt_reference":"MATERIALIZER-RECEIPT","occurred_at":"2026-08-31T12:34:00.000000Z",
                "line":[{"purchase_order_line_id":order.line_id,"quantity":"9.0000",
                    "location_id":"00000000-0000-0000-0000-000000000201"}]}}]))
            .send().await?;
        ensure!(receipt.status() == reqwest::StatusCode::OK, "the materializer Receipt command must return HTTP 200");
        let receipt: Value = receipt.json().await?;
        fs::write(cluster.resources.evidence.join("materializer-receipt.json"), serde_json::to_vec_pretty(&receipt)?)?;
        let receipt_id = receipt[0]["value"]["receipt_id"].as_str().context("the Receipt command returned its identity")?;
        ensure!(receipt.as_array().is_some_and(|items| items.len() == 1)
            && receipt[0]["request_id"] == "materializer-receipt"
            && receipt[0]["value"].as_object().is_some_and(|value| value.len() == 4)
            && receipt[0]["value"]["purchase_order_id"] == order.order_id.as_str()
            && receipt[0]["value"]["purchase_order_status"] == "complete"
            && receipt[0]["value"]["row_version"] == 3
            && receipt[0]["value"].get("acme_inspection_required").is_none()
            && receipt[0]["value"].get("acme_quality_status").is_none()
            && receipt_id.len() == 36 && receipt_id.bytes().all(|byte| byte == b'-'
                || (byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())),
            "the materializer Receipt command must return the exact base result");
        let detail_query = wamn_execution_contract::encode_read_query(
            &serde_json::Map::from_iter([("id".to_owned(), json!(order.order_id))]));
        let detail = http.get(format!("{endpoint}/acme/purchase_order/get?{detail_query}"))
            .header("Host", &cluster.inputs.route_host).bearer_auth(&token)
            .send().await?;
        ensure!(detail.status() == reqwest::StatusCode::OK,
            "the materializer Receipt detail read must return HTTP 200");
        let detail: Value = detail.json().await?;
        fs::write(cluster.resources.evidence.join("materializer-receipt-detail.json"),
            serde_json::to_vec_pretty(&detail)?)?;
        ensure!(detail.as_array().is_some_and(|items| items.len() == 1)
            && detail[0].get("request_id").is_none()
            && detail[0]["value"]["id"] == order.order_id.as_str()
            && detail[0]["value"]["row_version"] == 3
            && detail[0]["value"]["acme_inspection_required"] == true
            && detail[0]["value"]["acme_quality_status"] == "pending",
            "the separate materializer Receipt detail read must retain its expected state");
        Ok::<_, anyhow::Error>(MaterializerPhase {
            project_pg_url: database_url.to_owned(), nats_url: cluster.nats_url.clone(), receipt_id: receipt_id.to_owned(),
        })
    }).await.context("the materializer HTTP commands exceeded 90 seconds")?
        .map(|phase| (phase, update_trace, receipt_trace))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_requires_the_released_route_response() {
        let body = br#"{"error":{"code":"route-not-found"}}"#;
        validate_response(404, "application/json", body).unwrap();
        assert!(validate_response(200, "application/json", body).is_err());
        assert!(validate_response(404, "text/html", b"<h1>Not Found</h1>").is_err());
        assert!(
            validate_response(404, "application/json", br#"{"error":{"code":"other"}}"#).is_err()
        );
    }
}
