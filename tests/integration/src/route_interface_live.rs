//! A route answers every platform fixture operation kind the same way as the
//! one-node wiring it replaces.
//!
//! Every fixture attachment is a route. One local application serves each one
//! twice: at its own path through a one-node wiring of the same export, which
//! the test builds, and under `/route` as the route itself. Each case sends one
//! body to both paths and compares the two answers. The two widget-maker
//! projections are served by a route only: no wiring for them exists in the
//! release or the catalog, so their answers prove that a route runs no graph
//! walk. The last section shows the conditional read of a list route.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_catalog::{AttachmentTarget, ServingAttachment, WiringDocument};
use wamn_test_infrastructure::scratch::ScratchRoot;

use crate::local_application::{LocalApplication, LocalApplicationConfig, LocalPackage};

const ROUTE_PREFIX: &str = "/route";

/// The fixture operations that get no one-node wiring: only their routes
/// serve them.
const ROUTE_ONLY: &[&str] = &["widget_maker_list", "widget_maker_query"];

struct Paths {
    endpoint: String,
    host: String,
    bearer: String,
    client: reqwest::Client,
}

impl Paths {
    async fn post(&self, path: &str, body: &Value) -> anyhow::Result<(u16, Value)> {
        self.send(
            self.client
                .post(format!("{}{path}", self.endpoint))
                .json(body),
            path,
        )
        .await
    }

    /// Send the one item of a read as a GET, as its route is published.
    async fn get(&self, path: &str, body: &Value) -> anyhow::Result<(u16, Value)> {
        let item = body[0]
            .as_object()
            .context("a read sends one object item")?;
        let query = wamn_execution_contract::encode_read_query(item);
        self.send(
            self.client.get(format!("{}{path}?{query}", self.endpoint)),
            path,
        )
        .await
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        path: &str,
    ) -> anyhow::Result<(u16, Value)> {
        let response = request
            .header("Host", &self.host)
            .bearer_auth(&self.bearer)
            .send()
            .await
            .with_context(|| path.to_owned())?;
        let status = response.status().as_u16();
        let text = response.text().await?;
        let value = serde_json::from_str(&text)
            .with_context(|| format!("{path} answered {status} with {text}"))?;
        Ok((status, value))
    }

    /// Send one body through the wiring and then through the route, and
    /// require the same status and the same answer. A wiring is a POST. A
    /// route to a read is a GET, and every other route is a POST.
    async fn alike(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        let wiring = self.post(path, body).await?;
        let route = self.by_kind(&format!("{ROUTE_PREFIX}{path}"), body).await?;
        anyhow::ensure!(
            wiring == route,
            "{path} answered differently\n wiring: {wiring:?}\n  route: {route:?}"
        );
        Ok(wiring.1)
    }

    /// Send a GET to a route with an optional If-None-Match, and return the
    /// status, the ETag and the body text.
    async fn conditional(
        &self,
        path: &str,
        if_none_match: Option<&str>,
    ) -> anyhow::Result<(u16, Option<String>, String)> {
        let mut request = self
            .client
            .get(format!("{}{ROUTE_PREFIX}{path}", self.endpoint))
            .header("Host", &self.host)
            .bearer_auth(&self.bearer);
        if let Some(tag) = if_none_match {
            request = request.header("If-None-Match", tag);
        }
        let response = request.send().await.with_context(|| path.to_owned())?;
        let status = response.status().as_u16();
        let etag = response
            .headers()
            .get("etag")
            .map(|value| value.to_str().map(ToOwned::to_owned))
            .transpose()?;
        Ok((status, etag, response.text().await?))
    }

    /// Send one body through the route only.
    async fn route(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        let (status, value) = self.by_kind(&format!("{ROUTE_PREFIX}{path}"), body).await?;
        anyhow::ensure!(status == 200, "route {path} answered {status}: {value}");
        Ok(value)
    }
}

impl Paths {
    /// A route to a read carries no request identity, so an item without
    /// one is a read.
    async fn by_kind(&self, path: &str, body: &Value) -> anyhow::Result<(u16, Value)> {
        if body[0].get("request_id").is_none() {
            self.get(path, body).await
        } else {
            self.post(path, body).await
        }
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

/// Each fixture route attachment under `/route`, and at its own path an
/// attachment of a one-node wiring that calls the same export.
fn attachments(
    app: &std::path::Path,
) -> anyhow::Result<(BTreeMap<String, ServingAttachment>, Vec<WiringDocument>)> {
    let routes: BTreeMap<String, ServingAttachment> =
        wamn_schema_generator::route_schema::read_package_attachments(app)?;
    let declaration: Value = serde_json::from_slice(&std::fs::read(
        app.join("publication/components/fixture.json.in"),
    )?)?;
    let interface_version = declaration["interface-version"]
        .as_str()
        .context("the fixture declaration names its interface version")?;
    let mut attachments = BTreeMap::new();
    let mut wirings = Vec::new();
    for (id, attachment) in routes {
        let AttachmentTarget::Route {
            component,
            operation,
        } = attachment.target.clone()
        else {
            anyhow::bail!("fixture attachment {id} is not a route");
        };
        let mut route = attachment.clone();
        let path = route.definition["route"]["path"]
            .as_str()
            .with_context(|| format!("{id} names a path"))?
            .to_owned();
        route.definition["route"]["path"] = json!(format!("{ROUTE_PREFIX}{path}"));
        route.definition["id"] = json!(format!("route-{id}"));
        attachments.insert(format!("route-{id}"), route);

        let wiring_id = id.trim_end_matches("-http").replace('-', "_");
        if ROUTE_ONLY.contains(&wiring_id.as_str()) {
            continue;
        }
        wirings.push(WiringDocument::parse(&json!({
            "format-version": "0.1",
            "wiring-id": wiring_id,
            "version": 1,
            "entry": "operation",
            "nodes": {"operation": {
                "component": component,
                "interface-version": interface_version,
                "operation": operation,
                "terminal": "respond",
            }},
        }))?);
        let mut wired = attachment;
        wired.target = AttachmentTarget::Wiring {
            wiring_id,
            wiring_version: 1,
        };
        attachments.insert(id, wired);
    }
    Ok((attachments, wirings))
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
    let (attachments, wirings) = attachments(&app)?;
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
            wirings: &wirings,
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
    let got = paths.alike("/widget/get", &json!([{"id": id}])).await?;
    anyhow::ensure!(value(&got)?["id"] == id, "get reads the widget: {got}");
    paths.alike("/widget/query", &json!([{}])).await?;
    paths
        .alike("/widget/list", &json!([{"selector": {}}]))
        .await?;

    // A server-owned field is invalid input on both paths: the route input
    // schema admits no member the caller does not own.
    let invalid = paths
        .alike("/widget/get", &json!([{"id": id, "edit_version": 1}]))
        .await?;
    anyhow::ensure!(
        invalid["error"]["code"] == "schema-invalid"
            && invalid["error"]["data"]["pointer"] == "/0/edit_version",
        "a server-owned field is refused by the route schema: {invalid}"
    );

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
                "expected_edit_version": "1", "grade": "first",
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
    paths.alike("/widget/get", &json!([{"id": id}])).await?;

    // NO WALK. The projections have no wiring anywhere in this release.
    value(&paths.route("/widget_maker/list", &json!([{}])).await?)?;
    value(&paths.route("/widget_maker/query", &json!([{}])).await?)?;

    // CONDITIONAL READS. A list answers with a weak ETag from the versions of
    // the relations it reads. The same GET with that tag answers 304 with no
    // body and runs no read, and a write changes the tag.
    let (status, etag, _) = paths.conditional("/widget/query", None).await?;
    let etag = etag
        .filter(|_| status == 200)
        .with_context(|| format!("the list answers 200 with an ETag, not {status}"))?;
    anyhow::ensure!(etag.starts_with("W/\""), "a list tag is weak: {etag}");
    let (status, again, body) = paths.conditional("/widget/query", Some(&etag)).await?;
    anyhow::ensure!(
        status == 304 && again.as_deref() == Some(etag.as_str()) && body.is_empty(),
        "an unchanged list answers 304 with its tag and no body: {status} {again:?} {body}"
    );
    value(
        &paths
            .route(
                "/widget/create",
                &json!([{"request_id": "create-3", "idempotency_key": "create-3", "code": "standard"}]),
            )
            .await?,
    )?;
    let (status, changed, _) = paths.conditional("/widget/query", Some(&etag)).await?;
    anyhow::ensure!(
        status == 200 && changed.is_some() && changed.as_deref() != Some(etag.as_str()),
        "a write changes the list tag: {status} {changed:?} {etag}"
    );
    // A get answers a strong ETag from the revision of its record. The same
    // GET with that tag answers 304 with no body, and an update of the record
    // changes the tag.
    let get_path = format!(
        "/widget/get?{}",
        wamn_execution_contract::encode_read_query(
            json!({"id": other}).as_object().context("an item")?
        )
    );
    let (status, get_tag, body) = paths.conditional(&get_path, None).await?;
    let get_tag = get_tag
        .filter(|tag| status == 200 && tag.starts_with('"'))
        .with_context(|| format!("a get with a revision answers a strong ETag: {status}"))?;
    let (status, again, empty) = paths.conditional(&get_path, Some(&get_tag)).await?;
    anyhow::ensure!(
        status == 304 && again.as_deref() == Some(get_tag.as_str()) && empty.is_empty(),
        "an unchanged get answers 304 with its tag and no body: {status} {again:?} {empty}"
    );
    let other_version = edit_version(&serde_json::from_str(&body)?)?;
    value(
        &paths
            .route(
                "/widget/update",
                &json!([{
                    "request_id": "update-3", "id": other,
                    "expected_edit_version": other_version.to_string(),
                    "change": {"note": "tagged"},
                }]),
            )
            .await?,
    )?;
    let (status, changed, _) = paths.conditional(&get_path, Some(&get_tag)).await?;
    anyhow::ensure!(
        status == 200 && changed.is_some() && changed.as_deref() != Some(get_tag.as_str()),
        "an update changes the get tag: {status} {changed:?} {get_tag}"
    );

    application.shutdown().await?;
    Ok(())
}
