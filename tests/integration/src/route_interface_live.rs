//! A route answers every platform fixture operation kind the same way as the
//! one-node wiring it replaces.
//!
//! One local application serves each fixture attachment twice: at its own
//! path through its one-node wiring, and under `/route` through a route to the
//! same export. Each case sends one body to both paths and compares the two
//! answers. The two widget-maker projections are served by a route only: no
//! wiring for them exists in the release or the catalog, so their answers
//! prove that a route runs no graph walk.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_catalog::{AttachmentTarget, ServingAttachment};
use wamn_test_infrastructure::scratch::ScratchRoot;

use crate::local_application::{LocalApplication, LocalApplicationConfig, LocalPackage};

const ROUTE_PREFIX: &str = "/route";

/// The fixture wirings the release serves. The two widget-maker projections
/// are left out on purpose: only their routes serve them.
const WIRINGS: &[&str] = &[
    "widget_archive",
    "widget_create",
    "widget_delete",
    "widget_get",
    "widget_list",
    "widget_query",
    "widget_record_batch",
    "widget_tag_update",
    "widget_update",
];

struct Paths {
    endpoint: String,
    host: String,
    bearer: String,
    client: reqwest::Client,
}

impl Paths {
    async fn post(&self, path: &str, body: &Value) -> anyhow::Result<(u16, Value)> {
        let response = self
            .client
            .post(format!("{}{path}", self.endpoint))
            .header("Host", &self.host)
            .bearer_auth(&self.bearer)
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        let status = response.status().as_u16();
        let text = response.text().await?;
        let value = serde_json::from_str(&text)
            .with_context(|| format!("POST {path} answered {status} with {text}"))?;
        Ok((status, value))
    }

    /// Send one body through the wiring and then through the route, and
    /// require the same status and the same answer.
    async fn alike(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        let wiring = self.post(path, body).await?;
        let route = self.post(&format!("{ROUTE_PREFIX}{path}"), body).await?;
        anyhow::ensure!(
            wiring == route,
            "{path} answered differently\n wiring: {wiring:?}\n  route: {route:?}"
        );
        Ok(wiring.1)
    }

    /// Send one body through the route only.
    async fn route(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        let (status, value) = self.post(&format!("{ROUTE_PREFIX}{path}"), body).await?;
        anyhow::ensure!(status == 200, "route {path} answered {status}: {value}");
        Ok(value)
    }
}

/// The `value` of the one item of an answer.
fn value(answer: &Value) -> anyhow::Result<&Value> {
    answer[0]
        .get("value")
        .with_context(|| format!("the answer carries a value: {answer}"))
}

/// The `error` code of the one item of an answer.
fn refusal(answer: &Value) -> anyhow::Result<&str> {
    answer[0]["error"]["code"]
        .as_str()
        .with_context(|| format!("the answer carries a refusal: {answer}"))
}

fn edit_version(answer: &Value) -> anyhow::Result<i64> {
    let version = &value(answer)?["edit_version"];
    version
        .as_i64()
        .or_else(|| version.as_str().and_then(|text| text.parse().ok()))
        .with_context(|| format!("an edit version: {answer}"))
}

/// Each fixture attachment, plus a route copy of it under `/route`.
fn attachments(app: &std::path::Path) -> anyhow::Result<BTreeMap<String, ServingAttachment>> {
    let wirings: BTreeMap<String, ServingAttachment> =
        serde_json::from_slice(&std::fs::read(app.join("publication/attachments.json"))?)?;
    let mut attachments = wirings.clone();
    for (id, attachment) in wirings {
        let operation = attachment
            .registered_operation
            .clone()
            .with_context(|| format!("{id} registers its operation"))?;
        let mut route = attachment;
        route.target = AttachmentTarget::Route {
            component: "fixture".to_owned(),
            operation,
        };
        let path = route.definition["route"]["path"]
            .as_str()
            .with_context(|| format!("{id} names a path"))?
            .to_owned();
        route.definition["route"]["path"] = json!(format!("{ROUTE_PREFIX}{path}"));
        route.definition["id"] = json!(format!("route-{id}"));
        attachments.insert(format!("route-{id}"), route);
    }
    Ok(attachments)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT"]
async fn a_route_answers_every_operation_kind_as_its_one_node_wiring() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let app = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/platform_fixture")
        .canonicalize()?;
    let components = PathBuf::from(std::env::var("WAMN_APPLICATION_COMPONENTS")?);
    let flow_http = PathBuf::from(std::env::var("WAMN_FLOW_HTTP_COMPONENT")?);
    let attachments = attachments(&app)?;
    let application = LocalApplication::start(LocalApplicationConfig {
        system_database_url: system.url(),
        database_url: project.url(),
        scratch: scratch.path(),
        component_directory: &components,
        flow_http_wasm: &flow_http,
        tenant: "route-interface",
        org: "acme",
        project: "fixture",
        environment: "dev",
        schema: "inventory",
        caller_role: "route-caller",
        route_host: "fixture.local.test",
        packages: &[LocalPackage {
            root: &app,
            component: "fixture",
            wirings: WIRINGS,
        }],
        attachments: &attachments,
    })
    .await?;
    let paths = Paths {
        endpoint: application.endpoint.clone(),
        host: application.route_host.clone(),
        bearer: application.bearer.clone(),
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?,
    };

    // CREATE. The route replays the command the wiring created, from the same
    // claim record: the same id and the same creation time.
    let create = json!([{
        "request_id": "create", "idempotency_key": "create-1", "code": "standard",
    }]);
    let created = paths.alike("/widget/create", &create).await?;
    let id = value(&created)?["id"].clone();
    let version = edit_version(&created)?;
    let second = paths
        .route(
            "/widget/create",
            &json!([{"request_id": "create-2", "idempotency_key": "create-2", "code": "priority"}]),
        )
        .await?;
    let other = value(&second)?["id"].clone();
    anyhow::ensure!(other != id, "a new key creates a new widget: {second}");

    // GET, QUERY and PROJECTION reads answer alike.
    let got = paths
        .alike("/widget/get", &json!([{"request_id": "get", "id": id}]))
        .await?;
    anyhow::ensure!(value(&got)?["id"] == id, "get reads the widget: {got}");
    paths
        .alike("/widget/query", &json!([{"request_id": "query"}]))
        .await?;
    paths
        .alike(
            "/widget/list",
            &json!([{"request_id": "list", "selector": {}}]),
        )
        .await?;

    // A server-owned field is invalid input on both paths.
    let invalid = paths
        .alike(
            "/widget/get",
            &json!([{"request_id": "invalid", "id": id, "edit_version": 1}]),
        )
        .await?;
    refusal(&invalid)?;

    // UPDATE. A stale revision is refused alike, and each path advances the
    // revision by one.
    let updated = paths
        .post(
            "/widget/update",
            &json!([{
                "request_id": "update-1", "id": id, "expected_edit_version": version.to_string(),
                "change": {"note": "wiring"},
            }]),
        )
        .await?;
    anyhow::ensure!(edit_version(&updated.1)? == version + 1, "{updated:?}");
    let stale = json!([{
        "request_id": "stale", "id": id, "expected_edit_version": version.to_string(),
        "change": {"note": "stale"},
    }]);
    refusal(&paths.alike("/widget/update", &stale).await?)?;
    let updated = paths
        .route(
            "/widget/update",
            &json!([{
                "request_id": "update-2", "id": id, "expected_edit_version": (version + 1).to_string(),
                "change": {"note": "route"},
            }]),
        )
        .await?;
    anyhow::ensure!(edit_version(&updated)? == version + 2, "{updated}");
    let version = version + 2;

    // COMMAND. The route replays the batch the wiring recorded, and a stale
    // archive is refused alike before the route archives the widget.
    let batch = paths
        .alike(
            "/widget/record_batch",
            &json!([{"request_id": "batch", "value": {
                "idempotency_key": "batch-1", "note": null, "maker_id": null,
                "line": [{"widget_id": other, "amount": "1"}],
            }}]),
        )
        .await?;
    value(&batch)?;
    let archive = |expected: i64| json!([{"request_id": "archive", "id": id, "expected_edit_version": expected.to_string()}]);
    refusal(
        &paths
            .alike("/widget/archive", &archive(version - 1))
            .await?,
    )?;
    let archived = paths.route("/widget/archive", &archive(version)).await?;
    let version = edit_version(&archived)?;

    // DELETE. A stale delete is refused alike, the route deletes, and both
    // paths then read the same absence.
    let delete = |expected: i64| json!([{"request_id": "delete", "id": id, "expected_edit_version": expected.to_string()}]);
    refusal(&paths.alike("/widget/delete", &delete(version - 1)).await?)?;
    value(&paths.route("/widget/delete", &delete(version)).await?)?;
    paths
        .alike("/widget/get", &json!([{"request_id": "gone", "id": id}]))
        .await?;

    // NO WALK. The projections have no wiring anywhere in this release.
    value(
        &paths
            .route("/widget_maker/list", &json!([{"request_id": "makers"}]))
            .await?,
    )?;
    value(
        &paths
            .route(
                "/widget_maker/query",
                &json!([{"request_id": "maker-query"}]),
            )
            .await?,
    )?;

    application.shutdown().await?;
    Ok(())
}
