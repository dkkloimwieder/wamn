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

    /// Send a streamed read to a route, and return the status, the headers
    /// and the reply lines, each parsed as JSON.
    async fn streamed(
        &self,
        path: &str,
        item: &Value,
        if_none_match: Option<&str>,
    ) -> anyhow::Result<(u16, reqwest::header::HeaderMap, Vec<Value>)> {
        let query = wamn_execution_contract::encode_read_query(
            item.as_object().context("a read item is an object")?,
        );
        let mut request = self
            .client
            .get(format!("{}{ROUTE_PREFIX}{path}?{query}", self.endpoint))
            .header("Host", &self.host)
            .bearer_auth(&self.bearer);
        if let Some(tag) = if_none_match {
            request = request.header("If-None-Match", tag);
        }
        let response = request.send().await.with_context(|| path.to_owned())?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let text = response.text().await?;
        let lines = text
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<Vec<Value>, _>>()
            .with_context(|| format!("{path} answered {status} with {text}"))?;
        Ok((status, headers, lines))
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
    Ok((application, paths))
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
    let (application, paths) = start(system.url(), project.url(), &scratch).await?;

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
    // STREAMED READS (wamn-utci.2). The same query in the stream shape answers
    // one line for each row and one outcome line last, with the list's weak
    // ETag, and an unchanged load answers 304. A cap above the ceiling refuses
    // before the first row with the refusal a page answers.
    let page = paths.route("/widget/query", &json!([{}])).await?;
    let page_ids = value(&page)?["item"]
        .as_array()
        .context("a page carries its rows")?
        .iter()
        .map(|row| row["id"].clone())
        .collect::<Vec<_>>();
    anyhow::ensure!(page_ids.len() > 1, "the fixture holds more than one widget");
    let (status, headers, lines) = paths
        .streamed(
            "/widget/query",
            &json!({"limit": 1000, "shape": "stream"}),
            None,
        )
        .await?;
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    anyhow::ensure!(
        status == 200
            && header("content-type") == Some("application/x-ndjson")
            && header("cache-control") == Some("private, no-cache")
            && header("x-accel-buffering") == Some("no"),
        "a load answers 200 as private, unbuffered lines: {status} {headers:?}"
    );
    let load_tag = header("etag")
        .filter(|tag| tag.starts_with("W/\""))
        .context("a load carries the weak list ETag")?
        .to_owned();
    let (outcome, rows) = lines.split_last().context("a load ends in its outcome")?;
    anyhow::ensure!(
        rows.iter()
            .map(|line| line["row"]["id"].clone())
            .collect::<Vec<_>>()
            == page_ids
            && outcome["outcome"]["value"]["more"] == json!(false),
        "a load reads the page's rows and says no more exist: {lines:?}"
    );
    let (status, _, lines) = paths
        .streamed(
            "/widget/query",
            &json!({"limit": 1, "shape": "stream"}),
            None,
        )
        .await?;
    anyhow::ensure!(
        status == 200
            && lines.len() == 2
            && lines[0]["row"]["id"] == page_ids[0]
            && lines[1]["outcome"]["value"]["more"] == json!(true),
        "a load of one row says more exist: {lines:?}"
    );
    let (status, headers, lines) = paths
        .streamed(
            "/widget/query",
            &json!({"limit": 1000, "shape": "stream"}),
            Some(&load_tag),
        )
        .await?;
    anyhow::ensure!(
        status == 304 && lines.is_empty() && headers.get("etag").is_some(),
        "an unchanged load answers 304 with no body: {status} {lines:?}"
    );
    let (status, _, lines) = paths
        .streamed(
            "/widget/query",
            &json!({"limit": 100_001, "shape": "stream"}),
            None,
        )
        .await?;
    anyhow::ensure!(
        status == 200
            && lines.len() == 1
            && lines[0][0]["error"]["code"] == json!("invalid_input")
            && lines[0][0]["error"]["detail"]["maximum"] == json!("100000"),
        "a cap above the ceiling refuses before the first row: {status} {lines:?}"
    );
    let (status, _, lines) = paths
        .streamed(
            "/widget/query",
            &json!({"limit": 101, "shape": "page"}),
            None,
        )
        .await?;
    anyhow::ensure!(
        status == 400 && lines[0]["error"]["data"]["pointer"] == json!("/0/limit"),
        "the input schema refuses a page above its maximum: {status} {lines:?}"
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

/// Send a streamed read on a raw connection and read its head and first
/// rows, and nothing more: a client that holds its socket reads no row the
/// kernel did not already receive.
async fn open_load(
    address: &str,
    paths: &Paths,
    path: &str,
) -> anyhow::Result<tokio::net::TcpStream> {
    let mut socket = tokio::net::TcpStream::connect(address).await?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\n\r\n",
        paths.host, paths.bearer
    );
    tokio::io::AsyncWriteExt::write_all(&mut socket, request.as_bytes()).await?;
    let mut first = vec![0; 4096];
    let read = tokio::io::AsyncReadExt::read(&mut socket, &mut first).await?;
    anyhow::ensure!(
        first[..read].starts_with(b"HTTP/1.1 200"),
        "the load answers 200: {}",
        String::from_utf8_lossy(&first[..read])
    );
    Ok(socket)
}

/// The record-history actor of this test's own writes.
const ACTOR: &str =
    "SELECT set_config('app.user_id', '00000000-0000-4000-8000-000000000001', true)";

/// More widget makers than the sockets between the host and the client hold:
/// each row is about 1 KiB, and a load of 100,000 rows about 100 MiB.
async fn makers(database: &tokio_postgres::Client) -> anyhow::Result<()> {
    database
        .batch_execute(&format!(
            "BEGIN; {ACTOR}; \
             INSERT INTO inventory.widget_maker (name) \
             SELECT 'maker ' || n || repeat('x', 1000) FROM generate_series(1, 150000) n; \
             COMMIT"
        ))
        .await
        .map_err(|error| anyhow::anyhow!("insert the widget makers: {error:?}"))
}

/// Transactions open on the project database, other than this session's.
const OPEN_TRANSACTIONS: &str = "SELECT count(*) FROM pg_stat_activity \
     WHERE datname = current_database() AND pid <> pg_backend_pid() AND xact_start IS NOT NULL";

/// Wait until the number of open transactions satisfies `settled`.
async fn transactions(
    database: &tokio_postgres::Client,
    settled: impl Fn(i64) -> bool,
) -> anyhow::Result<i64> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let open: i64 = database.query_one(OPEN_TRANSACTIONS, &[]).await?.get(0);
        if settled(open) || tokio::time::Instant::now() > deadline {
            return Ok(open);
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// A streamed load ends its database query when it stops: at its cap, when
/// the client aborts it, and when its connection drops (wamn-utci.3). While a
/// client reads no more, the load holds one transaction open on the server,
/// and each stop ends that transaction.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT"]
async fn a_streamed_load_that_stops_ends_its_database_query() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let (application, paths) = start(system.url(), project.url(), &scratch).await?;
    let (database, connection) =
        tokio_postgres::connect(project.url(), tokio_postgres::NoTls).await?;
    let connection = tokio::spawn(connection);
    makers(&database).await?;
    let load = json!({"limit": 100_000, "shape": "stream"});
    let query = wamn_execution_contract::encode_read_query(load.as_object().context("an item")?);
    let path = format!("{ROUTE_PREFIX}/widget_maker/query?{query}");

    // CAP STOP. A load of ten rows reads eleven, says more exist, and ends.
    let (status, _, lines) = paths
        .streamed(
            "/widget_maker/query",
            &json!({"limit": 10, "shape": "stream"}),
            None,
        )
        .await?;
    anyhow::ensure!(
        status == 200 && lines.len() == 11 && lines[10]["outcome"]["value"]["more"] == json!(true),
        "a load of ten rows says more exist: {status} {lines:?}"
    );
    let open = transactions(&database, |open| open == 0).await?;
    anyhow::ensure!(open == 0, "the cap stop ends the query: {open} open");

    // ABORT. The client reads the head and the first rows of a load, then
    // closes its connection, as a browser does when a page aborts a fetch.
    let address = paths
        .endpoint
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned();
    let mut socket = open_load(&address, &paths, &path).await?;
    let open = transactions(&database, |open| open >= 1).await?;
    anyhow::ensure!(
        open >= 1,
        "a load the client does not read holds its transaction"
    );
    tokio::io::AsyncWriteExt::shutdown(&mut socket).await?;
    drop(socket);
    let open = transactions(&database, |open| open == 0).await?;
    anyhow::ensure!(open == 0, "an abort ends the query: {open} open");

    // DISCONNECT. The connection resets, as a dropped network does.
    let socket = open_load(&address, &paths, &path).await?;
    let open = transactions(&database, |open| open >= 1).await?;
    anyhow::ensure!(
        open >= 1,
        "a load the client does not read holds its transaction"
    );
    // A zero linger resets the connection at once, so the drop does not block.
    #[expect(deprecated, reason = "a zero linger is the reset this case needs")]
    socket.set_linger(Some(std::time::Duration::ZERO))?;
    drop(socket);
    let open = transactions(&database, |open| open == 0).await?;
    anyhow::ensure!(open == 0, "a disconnect ends the query: {open} open");

    // A client that reads to the end gets every row and the outcome line.
    let (status, _, lines) = paths.streamed("/widget_maker/query", &load, None).await?;
    let (outcome, rows) = lines.split_last().context("a load ends in its outcome")?;
    anyhow::ensure!(
        status == 200
            && rows.len() == 100_000
            && rows.iter().all(|line| line.get("row").is_some())
            && outcome["outcome"]["value"]["more"] == json!(true),
        "a full load reads its cap and says more exist: {status}, {} lines, {outcome}",
        lines.len()
    );

    drop(database);
    connection.await??;
    application.shutdown().await?;
    Ok(())
}

/// A streamed load reads one statement as it runs: the client has rows while
/// the statement's cursor is still open, fetched in batches. Every row comes
/// from the snapshot the statement started with, so a write that commits
/// during the load does not show in it (wamn-utci.5).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT"]
async fn a_streamed_load_reads_one_statement_as_of_its_start() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let (application, paths) = start(system.url(), project.url(), &scratch).await?;
    let (database, connection) =
        tokio_postgres::connect(project.url(), tokio_postgres::NoTls).await?;
    let connection = tokio::spawn(connection);
    makers(&database).await?;
    let load = json!({"limit": 100_000, "shape": "stream"});
    let query = wamn_execution_contract::encode_read_query(load.as_object().context("an item")?);

    // The client reads the first rows while the statement's cursor is open.
    let mut response = paths
        .client
        .get(format!(
            "{}{ROUTE_PREFIX}/widget_maker/query?{query}",
            paths.endpoint
        ))
        .header("Host", &paths.host)
        .bearer_auth(&paths.bearer)
        .send()
        .await?;
    anyhow::ensure!(response.status() == 200, "the load answers 200");
    let mut body = response
        .chunk()
        .await?
        .context("the load sends its first rows")?
        .to_vec();
    anyhow::ensure!(body.starts_with(b"{\"row\":"), "the first line is a row");
    let open = transactions(&database, |open| open >= 1).await?;
    anyhow::ensure!(
        open >= 1,
        "the statement is still open while the client holds rows"
    );
    let fetching: i64 = database
        .query_one(
            "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() \
             AND xact_start IS NOT NULL AND query LIKE 'FETCH FORWARD % FROM wamn_stream'",
            &[],
        )
        .await?
        .get(0);
    anyhow::ensure!(
        fetching == 1,
        "the load fetches its statement's rows in batches"
    );

    // A write commits during the load: every maker gets a new name.
    database
        .batch_execute(&format!(
            "BEGIN; {ACTOR}; UPDATE inventory.widget_maker SET name = 'renamed'; COMMIT"
        ))
        .await
        .map_err(|error| anyhow::anyhow!("rename the widget makers: {error:?}"))?;

    // The rest of the load reads the names as they were when it started.
    while let Some(chunk) = response.chunk().await? {
        body.extend_from_slice(&chunk);
    }
    let lines = std::str::from_utf8(&body)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<Value>, _>>()?;
    let (outcome, rows) = lines.split_last().context("a load ends in its outcome")?;
    anyhow::ensure!(
        rows.len() == 100_000
            && rows.iter().all(|line| {
                line["row"]["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("maker "))
            })
            && outcome["outcome"]["value"]["more"] == json!(true),
        "a load reads one snapshot: {} lines, {outcome}",
        lines.len()
    );
    // A new load reads the committed names.
    let (_, _, again) = paths
        .streamed(
            "/widget_maker/query",
            &json!({"limit": 10, "shape": "stream"}),
            None,
        )
        .await?;
    anyhow::ensure!(
        again[0]["row"]["name"] == json!("renamed"),
        "a new load reads the write: {again:?}"
    );

    drop(database);
    connection.await??;
    application.shutdown().await?;
    Ok(())
}
