//! A route answers every platform fixture operation kind the same way as the
//! one-node wiring it replaces.
//!
//! Every fixture attachment is a route. One local application serves each one
//! twice: at its own path through a one-node wiring of the same export, which
//! the test builds, and under `/route` as the route itself. Each case sends one
//! body to both paths and compares the two answers. The two widget-maker
//! projections are served by a route only: no wiring for them exists in the
//! release or the catalog, so their answers prove that a route runs no graph
//! walk. The later sections show the conditional read of a list route, the
//! page of a query, the ceiling, and each scope filter mode, the search and
//! the band.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_catalog::{AttachmentTarget, ServingAttachment, WiringDocument};
use wamn_test_infrastructure::scratch::ScratchRoot;

use crate::local_application::{LocalApplication, LocalApplicationConfig, LocalPackage};

const ROUTE_PREFIX: &str = "/route";

/// An id that names no row.
const MISSING: &str = "00000000-0000-4000-8000-00000000dead";

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

/// Require the one item of an answer to refuse with exactly this code and
/// detail.
fn refused(answer: &Value, code: &str, detail: &Value) -> anyhow::Result<()> {
    anyhow::ensure!(
        refusal(answer)? == code && &answer[0]["error"]["detail"] == detail,
        "the answer refuses with {code} {detail}: {answer}"
    );
    Ok(())
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

/// Start the fixture application with every attachment served twice, and
/// the client that calls it.
async fn start(
    system_url: &str,
    project_url: &str,
    scratch: &ScratchRoot,
) -> anyhow::Result<(LocalApplication, Paths)> {
    let app = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/platform_fixture")
        .canonicalize()?;
    let components = PathBuf::from(std::env::var("WAMN_APPLICATION_COMPONENTS")?);
    let flow_http = PathBuf::from(std::env::var("WAMN_FLOW_HTTP_COMPONENT")?);
    let (attachments, wirings) = attachments(&app)?;
    let application = LocalApplication::start(LocalApplicationConfig {
        system_database_url: system_url,
        database_url: project_url,
        scratch: scratch.path(),
        component_directory: &components,
        flow_http_wasm: &flow_http,
        tenant: "route-interface",
        org: "acme",
        project: "fixture",
        environment: "dev",
        schema: "inventory",
        caller_role: "admin",
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
    Ok((application, paths))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN"]
async fn a_route_answers_every_operation_kind_as_its_one_node_wiring() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
        "WAMN_NATIVE_C_NATS_BIN",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let (application, paths) = start(system.url(), project.url(), &scratch).await?;

    // CREATE. The route replays the command the wiring created, from the same
    // write log record: the same id and the same creation time.
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
    // A second widget with a taken code breaks the unique key the create
    // names.
    refused(
        &paths
            .alike(
                "/widget/create",
                &json!([{"request_id": "taken", "idempotency_key": "taken", "code": "priority"}]),
            )
            .await?,
        "unique_violation",
        &json!({"constraint": "widget_code_key", "field": "code"}),
    )?;

    // GET, QUERY and PROJECTION reads answer alike.
    let got = paths.alike("/widget/get", &json!([{"id": id}])).await?;
    anyhow::ensure!(value(&got)?["id"] == id, "get reads the widget: {got}");
    paths.alike("/widget/query", &json!([{}])).await?;
    paths
        .alike("/widget/list", &json!([{"selector": {}}]))
        .await?;
    refused(
        &paths
            .alike("/widget/query", &json!([{"cursor": "not-a-cursor"}]))
            .await?,
        "invalid_input",
        &json!({"field": "cursor"}),
    )?;
    refused(
        &paths
            .alike("/widget_maker/get", &json!([{"id": MISSING}]))
            .await?,
        "not_found",
        &json!({"field": "id", "id": MISSING}),
    )?;

    // The generated operation type refuses fields the caller does not own.
    let invalid = paths
        .route("/widget/get", &json!([{"id": id, "edit_version": 1}]))
        .await?;
    refused(&invalid, "invalid_input", &json!({"field": "input"}))?;

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
    refused(
        &paths.alike("/widget/update", &stale).await?,
        "concurrency_conflict",
        &json!({
            "expected_row_version": version.to_string(),
            "observed_row_version": (version + 1).to_string(),
        }),
    )?;
    let missing = json!([{
        "request_id": "missing", "id": MISSING, "expected_edit_version": "1",
        "change": {"note": "missing"},
    }]);
    refused(
        &paths.alike("/widget/update", &missing).await?,
        "not_found",
        &json!({"field": "id", "id": MISSING}),
    )?;
    refused(
        &paths
            .alike(
                "/widget_tag/update",
                &json!([{
                    "request_id": "missing", "id": MISSING, "expected_edit_version": "1",
                    "change": {"label": "missing"},
                }]),
            )
            .await?,
        "not_found",
        &json!({"field": "id", "id": MISSING}),
    )?;
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
    // The delete statement reports the revision it read, so the conflict
    // carries it.
    refused(
        &paths.alike("/widget/delete", &delete(version - 1)).await?,
        "concurrency_conflict",
        &json!({
            "expected_row_version": (version - 1).to_string(),
            "observed_row_version": version.to_string(),
        }),
    )?;
    value(&paths.route("/widget/delete", &delete(version)).await?)?;
    let absent = json!({"field": "id", "id": id});
    refused(
        &paths.alike("/widget/get", &json!([{"id": id}])).await?,
        "not_found",
        &absent,
    )?;
    // A delete is keyed by its request_id, so a second delete is a new request.
    refused(
        &paths
            .alike(
                "/widget/delete",
                &json!([{"request_id": "delete-again", "id": id, "expected_edit_version": version.to_string()}]),
            )
            .await?,
        "not_found",
        &absent,
    )?;
    // Its code is free again, and a maker that does not exist breaks the
    // foreign key the create names.
    refused(
        &paths
            .alike(
                "/widget/create",
                &json!([{
                    "request_id": "no-maker", "idempotency_key": "no-maker",
                    "code": "standard", "maker_id": MISSING,
                }]),
            )
            .await?,
        "foreign_key_violation",
        &json!({"constraint": "widget_maker_id_fkey", "field": "maker_id"}),
    )?;

    // NO WALK. The projections have no wiring anywhere in this release.
    value(&paths.route("/widget_maker/list", &json!([{}])).await?)?;
    value(&paths.route("/widget_maker/query", &json!([{}])).await?)?;

    // CONDITIONAL READS. A list answers with a weak ETag from the versions of
    // the relations it reads. The same GET with that tag answers 304 with no
    // body, and a write changes the tag.
    let mut tags = Vec::new();
    for path in ["/widget/query", "/widget_maker/list"] {
        let (status, etag, _) = paths.conditional(path, None).await?;
        let etag = etag
            .filter(|_| status == 200)
            .with_context(|| format!("the list answers 200 with an ETag, not {status}"))?;
        anyhow::ensure!(etag.starts_with("W/\""), "a list tag is weak: {etag}");
        let (status, again, body) = paths.conditional(path, Some(&etag)).await?;
        anyhow::ensure!(
            status == 304 && again.as_deref() == Some(etag.as_str()) && body.is_empty(),
            "an unchanged list answers 304 with its tag and no body: {status} {again:?} {body}"
        );
        tags.push((path, etag));
    }
    value(
        &paths
            .route(
                "/widget/create",
                &json!([{"request_id": "create-3", "idempotency_key": "create-3", "code": "standard"}]),
            )
            .await?,
    )?;
    for (path, etag) in tags {
        let (status, changed, _) = paths.conditional(path, Some(&etag)).await?;
        anyhow::ensure!(
            status == 200 && changed.is_some() && changed.as_deref() != Some(etag.as_str()),
            "a write changes the list tag: {status} {changed:?} {etag}"
        );
    }
    // ONE PAGE (wamn-cg5e4). A query answers its page as one JSON body: its
    // rows, and the cursor when more rows exist. A limit above the declared
    // maximum refuses before the query runs.
    let query_path = format!("{ROUTE_PREFIX}/widget/query");
    let (status, page) = paths.get(&query_path, &json!([{"limit": 1}])).await?;
    anyhow::ensure!(
        status == 200
            && value(&page)?["item"]
                .as_array()
                .is_some_and(|rows| rows.len() == 1)
            && value(&page)?["next_cursor"].is_string(),
        "a page of one row carries the cursor of the next: {status} {page}"
    );
    let (status, refused_page) = paths.get(&query_path, &json!([{"limit": 101}])).await?;
    anyhow::ensure!(
        status == 400 && refused_page["error"]["data"]["pointer"] == json!("/0/limit"),
        "the input schema refuses a page above its maximum: {status} {refused_page}"
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

/// A read above the platform ceiling of 50,000 rows refuses before the query
/// runs (wamn-cg5e4). The entry refuses it with the limit refusal, whose
/// maximum is the ceiling. A query's declared maximum of 100 refuses first.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN"]
async fn a_read_of_100000_rows_refuses_at_the_ceiling() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
        "WAMN_NATIVE_C_NATS_BIN",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let (application, paths) = start(system.url(), project.url(), &scratch).await?;
    let load = json!([{"limit": 100_000}]);

    // The projection declares no limit, so the ceiling refuses the load.
    let list = format!("{ROUTE_PREFIX}/widget_maker/list");
    let (status, answer) = paths.get(&list, &load).await?;
    anyhow::ensure!(status == 200, "the refusal answers 200: {status} {answer}");
    refused(
        &answer,
        "invalid_input",
        &json!({"field": "limit", "minimum": "1", "maximum": "50000", "observed": "100000"}),
    )?;
    // A limit at the ceiling passes it, and is not the ceiling refusal.
    let (status, answer) = paths.get(&list, &json!([{"limit": 50_000}])).await?;
    anyhow::ensure!(
        answer[0]["error"]["detail"]["maximum"] != json!("50000"),
        "a limit at the ceiling is not the ceiling refusal: {status} {answer}"
    );
    // The query's declared maximum refuses before the ceiling.
    let (status, answer) = paths
        .get(&format!("{ROUTE_PREFIX}/widget_maker/query"), &load)
        .await?;
    anyhow::ensure!(
        status == 400 && answer["error"]["data"]["pointer"] == json!("/0/limit"),
        "the query's maximum refuses a load of 100,000 rows: {status} {answer}"
    );

    application.shutdown().await?;
    Ok(())
}

/// The names of a page's rows under one member, sorted.
fn names(answer: &Value, member: &str) -> anyhow::Result<Vec<String>> {
    let mut names = value(answer)?["item"]
        .as_array()
        .with_context(|| format!("a page of rows: {answer}"))?
        .iter()
        .map(|row| row[member].as_str().map(ToOwned::to_owned))
        .collect::<Option<Vec<_>>>()
        .with_context(|| format!("each row names its {member}: {answer}"))?;
    names.sort();
    Ok(names)
}

/// Each new filter mode, the server search and the band read the right rows
/// from Postgres (wamn-3nsf.6). A widget matches its note by prefix and its
/// maker by whether it is empty. A maker read applies its band of the last 30
/// days until the request sends one, and its search finds a part of the name
/// in any case.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN"]
async fn each_filter_mode_the_search_and_the_band_read_their_rows() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
        "WAMN_NATIVE_C_NATS_BIN",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let (application, paths) = start(system.url(), project.url(), &scratch).await?;
    let (database, connection) =
        tokio_postgres::connect(project.url(), tokio_postgres::NoTls).await?;
    let connection = tokio::spawn(connection);
    // The seed states each creation time, which the stamp trigger would
    // replace, so it runs without triggers.
    database
        .batch_execute(
            "BEGIN; SET LOCAL session_replication_role = replica; \
             INSERT INTO inventory.widget_maker (name, created_at) VALUES \
               ('Acme Old', now() - interval '40 days'), \
               ('Acme New', now() - interval '1 day'), \
               ('Globex', now() - interval '2 days'); \
             INSERT INTO inventory.widget (code, note, maker_id) VALUES \
               ('priority', 'N-1', (SELECT id FROM inventory.widget_maker WHERE name = 'Acme New')), \
               ('standard', 'M-2', NULL); \
             COMMIT"
        )
        .await
        .map_err(|error| anyhow::anyhow!("insert the makers and widgets: {error:?}"))?;
    let makers = "/widget_maker/query";
    let widgets = "/widget/query";
    let since = (chrono::Utc::now() - chrono::Duration::days(60)).to_rfc3339();

    // The band reads the last 30 days until the request states its start.
    let default = paths.route(makers, &json!([{}])).await?;
    anyhow::ensure!(
        names(&default, "name")? == ["Acme New", "Globex"],
        "{default}"
    );
    let wide = paths
        .route(makers, &json!([{"filter": {"created_at": {"min": since}}}]))
        .await?;
    anyhow::ensure!(
        names(&wide, "name")? == ["Acme New", "Acme Old", "Globex"],
        "{wide}"
    );
    let (status, answer) = paths
        .get(
            &format!("{ROUTE_PREFIX}{makers}"),
            &json!([{"filter": {"created_at": {"max": since}}}]),
        )
        .await?;
    anyhow::ensure!(
        status != 200,
        "a band without its start is refused: {answer}"
    );

    // The search finds a part of the name in any case, inside the band.
    let found = paths
        .route(
            makers,
            &json!([{"search": "acme", "filter": {"created_at": {"min": since}}}]),
        )
        .await?;
    anyhow::ensure!(
        names(&found, "name")? == ["Acme New", "Acme Old"],
        "{found}"
    );

    // A prefix matches the start of the note, and is-null the empty maker.
    // The fixture holds two widget codes, so two widgets carry the cases.
    for (filter, expected) in [
        (json!({"note": ["N-"]}), vec!["priority"]),
        (json!({"note": ["-1"]}), vec![]),
        (json!({"maker_id": true}), vec!["standard"]),
        (json!({"maker_id": false}), vec!["priority"]),
        (json!({"note": ["M-"], "maker_id": true}), vec!["standard"]),
        (json!({"note": ["N-"], "maker_id": true}), vec![]),
    ] {
        let answer = paths.route(widgets, &json!([{"filter": filter}])).await?;
        anyhow::ensure!(names(&answer, "code")? == expected, "{filter}: {answer}");
    }

    drop(database);
    connection.await??;
    application.shutdown().await?;
    Ok(())
}

/// A composed read adds no transaction statements (wamn-5x0tt).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN"]
async fn composed_reads_use_one_statement_plus_optional_wal() -> anyhow::Result<()> {
    const REQUESTS: i64 = 10;
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
        "WAMN_NATIVE_C_NATS_BIN",
    ]);
    let mut server = wamn_test_postgres::start(&[
        ("shared_preload_libraries", "pg_stat_statements"),
        ("pg_stat_statements.track", "all"),
    ])?;
    let system = server.create_database("statement_system")?;
    let project = server.create_database("statement_project")?;
    let scratch = ScratchRoot::create()?;
    let (application, paths) = start(system.url(), project.url(), &scratch).await?;
    let result = async {
        let (database, connection) =
            tokio_postgres::connect(project.url(), tokio_postgres::NoTls).await?;
        let connection = tokio::spawn(connection);
        database
            .batch_execute("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
            .await?;
        let created = paths
            .route(
                "/widget/create",
                &json!([{
                    "request_id": "count", "idempotency_key": "count", "code": "standard",
                }]),
            )
            .await?;
        let id = value(&created)?["id"].clone();
        let get = json!([{"id": id}]);
        let query = json!([{"limit": 10}]);
        let projection = json!([{}]);
        // Warm the prepared statements and authentication caches before counting.
        for _ in 0..3 {
            paths.route("/widget/get", &get).await?;
            paths.route("/widget/query", &query).await?;
            paths.route("/widget_maker/list", &projection).await?;
        }
        for (path, body, expected_wal) in [
            ("/widget/get", &get, 0),
            ("/widget/query", &query, REQUESTS),
            ("/widget_maker/list", &projection, REQUESTS),
        ] {
            database
                .query_one("SELECT pg_stat_statements_reset()", &[])
                .await?;
            for _ in 0..REQUESTS {
                value(&paths.route(path, body).await?)?;
            }
            // Exclude this observer's superuser session, but count every guest SQL call.
            let statements = database
                .query(
                    "SELECT query, calls FROM pg_stat_statements \
                 WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) \
                 AND userid <> (SELECT oid FROM pg_roles WHERE rolname = current_user) \
                 ORDER BY query",
                    &[],
                )
                .await?;
            let mut calls = 0;
            let mut wal = 0;
            for row in statements {
                let sql: String = row.get(0);
                let count: i64 = row.get(1);
                eprintln!("wamn-5x0tt {path}: calls={count} sql={sql}");
                calls += count;
                if sql.contains("pg_current_wal_lsn") {
                    wal += count;
                }
            }
            anyhow::ensure!(
                wal == expected_wal,
                "{path}: WAL calls {wal}, expected {expected_wal}"
            );
            anyhow::ensure!(
                calls == REQUESTS + expected_wal,
                "{path}: {calls} SQL calls for {REQUESTS} requests, expected {}",
                REQUESTS + expected_wal
            );
            eprintln!(
                "wamn-5x0tt {path}: {} calls/request, {} WAL calls/request",
                calls / REQUESTS,
                wal / REQUESTS
            );
        }
        drop(database);
        connection.await??;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    application.shutdown().await?;
    result
}
