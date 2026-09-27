//! The route intent record in the cloud, through the host path (`wamn-an24.3`).
//!
//! The echo guest serves a create route and a get route. The host binds its
//! intent store to `wamn_run.intents`, and each call enters through the
//! delivery bridge, as an attachment request does.

use std::sync::Arc;

use serde_json::{Value, json};
use wamn_engine::operation::intent::item_input_hash;
use wamn_engine::router_delivery::{DeliveryOutcome, DeliveryRequest, RouteDelivery as _, Source};
use wamn_execution_host::{OperationHost, RouterDeliveryBridge, WiringDelivery};
use wamn_run_state::intent_sql::{
    environment_resolve_intent_sql, environment_uncertain_intents_sql,
};
use wamn_run_state::intent_store::{Begun, Intent};
use wamn_runtime::plugins::wamn_jetstream::WamnJetstream;

use super::{Node, RouterDriver, TENANT, echo_component};

const CREATE: &str = "automation:echo/create@1.0.0";
const GET: &str = "automation:echo/get@1.0.0";

pub(super) fn node() -> Node {
    let operation = json!({"input-ports":[{"name":"input","schema":{}}],
        "output-ports":[{"name":"main","schema":{}}],"parameters":[]});
    let attachment = |id: &str, operation: &str| {
        json!({"kind":"http","package-id":"automation","component":"echo","operation":operation,
               "definition":{"id":id,"kind":"http","run-deadline-ms":30000},
               "definition-hash":
                   "sha256:5555555555555555555555555555555555555555555555555555555555555555",
               "auth-policy":{"modes":["none"]}})
    };
    Node {
        bytes: echo_component(&[CREATE, GET]),
        declaration: json!({
            "scope": {"tenant-id":TENANT,"package-id":"automation","package-version":"1.0.0"},
            "component":"echo","interface-version":"0.1.0",
            "operations": {(CREATE): operation, (GET): operation},
            "connections":[]
        }),
        operation: CREATE.to_owned(),
        params: json!({}),
        operations: json!({(CREATE):{"statements":{}},(GET):{"statements":{}}}),
        routes: json!([
            {"package-id":"automation","component":"echo","operation":CREATE,"kind":"create"},
            {"package-id":"automation","component":"echo","operation":GET,"kind":"get"}
        ]),
        attachments: json!({
            "echo-create": attachment("echo-create", CREATE),
            "echo-get": attachment("echo-get", GET)
        }),
    }
}

/// One item of a route call.
fn item(key: &str, grams: u32) -> Value {
    json!({"request_id": key, "value": {"grams": grams}})
}

/// Deliver `items` to `attachment` and return the answer's items.
async fn call(
    bridge: &RouterDeliveryBridge,
    attachment: &str,
    delivery: &str,
    items: &Value,
) -> Value {
    let report = bridge
        .deliver(
            DeliveryRequest {
                source: Source::Attachment(attachment.to_owned()),
                delivery_id: delivery.to_owned(),
                payload: items.to_string(),
                caller: None,
                trace: None,
                parent_causation: None,
                if_none_match: None,
            },
            None,
        )
        .await;
    match report.outcome {
        Ok(DeliveryOutcome::Respond(body)) => serde_json::from_str(&body).expect("a JSON answer"),
        other => panic!("{attachment} answered {other:?}"),
    }
}

async fn intent_rows(admin: &tokio_postgres::Client) -> i64 {
    admin
        .query_one("SELECT count(*) FROM wamn_run.intents", &[])
        .await
        .expect("count the intents")
        .get(0)
}

pub(super) async fn run(
    admin: &mut tokio_postgres::Client,
    operations: &Arc<OperationHost>,
    driver: &Arc<RouterDriver>,
    jetstream: &Arc<WamnJetstream>,
) -> anyhow::Result<()> {
    let bridge = RouterDeliveryBridge::new(
        Arc::clone(operations),
        Some(Arc::clone(driver) as Arc<dyn WiringDelivery>),
        Arc::clone(jetstream),
        "default",
    )?;

    // A repeated create answers the first outcome and adds no row.
    let first = call(&bridge, "echo-create", "d-1", &json!([item("k-1", 1250)])).await;
    assert_eq!(first, json!([item("k-1", 1250)]));
    assert_eq!(intent_rows(admin).await, 1);
    let again = call(&bridge, "echo-create", "d-2", &json!([item("k-1", 1250)])).await;
    assert_eq!(again, first, "a repeated key answers its stored outcome");
    assert_eq!(intent_rows(admin).await, 1, "a repeated key adds no row");

    // A changed input under the same key answers idempotency_conflict.
    let changed = call(&bridge, "echo-create", "d-3", &json!([item("k-1", 9)])).await;
    assert_eq!(
        changed[0]["error"]["code"], "idempotency_conflict",
        "{changed}"
    );

    // A get logs nothing.
    let read = call(&bridge, "echo-get", "d-4", &json!([item("k-2", 1)])).await;
    assert_eq!(read, json!([item("k-2", 1)]));
    assert_eq!(intent_rows(admin).await, 1, "a get logs no intent");

    // A key that began and never finished lists as uncertain, the statements
    // of `wamn-ctl intents` list and resolve it, and the next call answers
    // intent-resolved.
    let open = item("k-open", 2);
    let release = operations.release_identity().manifest_digest.to_string();
    let begun = operations
        .intents()
        .expect("the host has an intent store")
        .begin(&Intent {
            tenant: TENANT,
            release: &release,
            package: "automation",
            operation: CREATE,
            idempotency_key: "k-open",
            input_hash: &item_input_hash(&open),
            deadline_ms: 1_000,
        })
        .await?;
    let Begun::New(id) = begun else {
        panic!("expected a new intent, got {begun:?}");
    };
    let row_id: i64 = id.0.parse()?;
    let transaction = admin.transaction().await?;
    transaction
        .execute(
            "SELECT set_config('search_path', 'wamn_run', true), \
                    set_config('app.tenant', $1, true)",
            &[&TENANT],
        )
        .await?;
    let listed: Vec<(i64, String)> = transaction
        .query(&environment_uncertain_intents_sql(), &[&"test"])
        .await?
        .iter()
        .map(|row| (row.get(0), row.get(5)))
        .collect();
    assert_eq!(listed, [(row_id, "k-open".to_owned())]);
    assert!(
        transaction
            .query(&environment_uncertain_intents_sql(), &[&"prod"])
            .await?
            .is_empty(),
        "another environment lists nothing"
    );
    assert_eq!(
        transaction
            .execute(
                &environment_resolve_intent_sql(),
                &[&row_id, &"operator-judgment", &"test"],
            )
            .await?,
        1
    );
    transaction.commit().await?;
    let resolved = call(&bridge, "echo-create", "d-5", &json!([open])).await;
    assert_eq!(
        resolved[0]["error"]["code"], "intent-resolved",
        "{resolved}"
    );
    assert_eq!(resolved[0]["error"]["detail"]["basis"], "operator-judgment");
    Ok(())
}
