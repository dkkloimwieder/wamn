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
use base64::Engine as _;
use serde_json::{Value, json};
use wamn_catalog::ServingAttachment;
use wamn_test_infrastructure::scratch::ScratchRoot;

use crate::local_application::{LocalApplication, LocalApplicationConfig, LocalPackage};

const ROUTE_PREFIX: &str = "/route";

/// An id that names no row.
const MISSING: &str = "00000000-0000-4000-8000-00000000dead";

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
        let wiring = self.by_kind(path, body).await?;
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
fn attachments(app: &std::path::Path) -> anyhow::Result<BTreeMap<String, ServingAttachment>> {
    let routes = wamn_schema_generator::route_schema::read_package_attachments(app)?;
    let mut attachments = BTreeMap::new();
    for (id, attachment) in routes {
        let mut alias = attachment.clone();
        let path = alias.definition["route"]["path"]
            .as_str()
            .context("route path")?
            .to_owned();
        alias.definition["route"]["path"] = json!(format!("{ROUTE_PREFIX}{path}"));
        alias.definition["id"] = json!(format!("route-{id}"));
        attachments.insert(format!("route-{id}"), alias);
        attachments.insert(id, attachment);
    }
    Ok(attachments)
}

/// Start the fixture application with every attachment served twice, and
/// the client that calls it.
async fn start(
    system_url: &str,
    project_url: &str,
    scratch: &ScratchRoot,
    caller_role: &str,
) -> anyhow::Result<(LocalApplication, Paths)> {
    let app = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/platform_fixture")
        .canonicalize()?;
    let components = PathBuf::from(std::env::var("WAMN_APPLICATION_COMPONENTS")?);
    let flow_http = PathBuf::from(std::env::var("WAMN_FLOW_HTTP_COMPONENT")?);
    let attachments = attachments(&app)?;
    std::fs::write(
        scratch.path().join("environment.k"),
        include_str!("../fixtures/local-environments/platform.k"),
    )?;
    let application = LocalApplication::start(LocalApplicationConfig {
        system_database_url: system_url,
        database_url: project_url,
        scratch: scratch.path(),
        component_directory: &components,
        flow_http_wasm: &flow_http,
        env_key: "route-interface",
        org: "acme",
        project: "fixture",
        environment: "dev",
        schema: "inventory",
        caller_role,
        route_host: "fixture.local.test",
        packages: &[LocalPackage {
            root: &app,
            component: "fixture",
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
async fn health_head_needs_no_credential() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
        "WAMN_NATIVE_C_NATS_BIN",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let (application, paths) = start(system.url(), project.url(), &scratch, "admin").await?;
    let response = paths
        .client
        .head(format!("{}/api/health", paths.endpoint))
        .header("Host", &paths.host)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(response.bytes().await?.is_empty());
    application.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN"]
async fn a_route_answers_every_operation_kind_consistently_across_aliases() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
        "WAMN_NATIVE_C_NATS_BIN",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let (application, paths) = start(system.url(), project.url(), &scratch, "admin").await?;

    // CREATE. The route replays the command the wiring created, from the same
    // write log record: the same id and the same creation time.
    let create = json!([{
        "request_id": "create", "id": uuid::Uuid::now_v7().to_string(), "idempotency_key": "create-1", "code": "standard",
    }]);
    let created = paths.alike("/widget/create", &create).await?;
    let id = value(&created)?["id"].clone();
    anyhow::ensure!(id == create[0]["id"], "create keeps the supplied id");
    let version = edit_version(&created)?;
    refused(
        &paths
            .alike(
                "/widget/create",
                &json!([{
                    "request_id": "duplicate-id", "idempotency_key": "duplicate-id",
                    "id": id, "code": "priority",
                }]),
            )
            .await?,
        "unique_violation",
        &json!({"constraint": "widget_id_pkey"}),
    )?;

    let second = paths
        .route(
            "/widget/create",
            &json!([{"request_id": "create-2", "id": uuid::Uuid::now_v7().to_string(), "idempotency_key": "create-2", "code": "priority"}]),
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
                &json!([{"request_id": "taken", "id": uuid::Uuid::now_v7().to_string(), "idempotency_key": "taken", "code": "priority"}]),
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
                    "request_id": "no-maker", "id": uuid::Uuid::now_v7().to_string(), "idempotency_key": "no-maker",
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
                &json!([{"request_id": "create-3", "id": uuid::Uuid::now_v7().to_string(), "idempotency_key": "create-3", "code": "standard"}]),
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
    let (application, paths) = start(system.url(), project.url(), &scratch, "admin").await?;
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
    let (application, paths) = start(system.url(), project.url(), &scratch, "admin").await?;
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
    let (application, paths) = start(system.url(), project.url(), &scratch, "admin").await?;
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
                    "request_id": "count", "id": uuid::Uuid::now_v7().to_string(), "idempotency_key": "count", "code": "standard",
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

/// One feed read of the widget list: the changes in the scope of `filter`
/// since `cursor`.
async fn widget_changes(paths: &Paths, filter: &Value, cursor: &str) -> anyhow::Result<Value> {
    let answer = paths
        .route(
            "/widget/query",
            &json!([{"filter": filter, "feed": {"cursor": cursor}}]),
        )
        .await?;
    Ok(value(&answer)?.clone())
}

/// The snapshot xmin in the opaque cursor that the host returns.
fn feed_xmin(cursor: &str) -> anyhow::Result<i64> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(cursor)?;
    let cursor = std::str::from_utf8(&bytes)?;
    cursor
        .split(':')
        .nth(1)
        .context("a cursor transaction")?
        .parse()
        .context("snapshot xmin")
}

/// All changes, including deletions, carry the statement's one snapshot xmin.
fn feed_versions(changes: &Value) -> anyhow::Result<Option<i64>> {
    let rows = changes["rows"].as_array().context("feed rows")?;
    let mut snapshot = None;
    for row in rows {
        let version: i64 = row["row_version"]
            .as_str()
            .context("an int64 row_version")?
            .parse()?;
        anyhow::ensure!(
            snapshot.is_none_or(|xmin| xmin == version),
            "one snapshot per statement: {changes}"
        );
        snapshot = Some(version);
        if let Some(data) = row.get("data") {
            anyhow::ensure!(
                data["row_version"] == row["row_version"],
                "data uses the same snapshot: {row}"
            );
        }
    }
    if changes["complete"] == true
        && let Some(version) = snapshot
    {
        let cursor = changes["next_cursor"].as_str().context("a next cursor")?;
        anyhow::ensure!(
            version == feed_xmin(cursor)?,
            "the version and cursor share xmin: {changes}"
        );
    }
    Ok(snapshot)
}

/// The change of one id in a feed answer, which must carry it once.
fn change_of<'a>(changes: &'a Value, id: &Value) -> anyhow::Result<&'a Value> {
    let rows = changes["rows"]
        .as_array()
        .with_context(|| format!("the changes carry rows: {changes}"))?;
    let found = rows
        .iter()
        .filter(|row| &row["id"] == id)
        .collect::<Vec<_>>();
    match found.as_slice() {
        [change] => Ok(change),
        _ => anyhow::bail!(
            "{id} appears once in the changes, not {}: {changes}",
            found.len()
        ),
    }
}

/// A widget the route creates, and its revision.
async fn create_widget(paths: &Paths, code: &str, note: &str) -> anyhow::Result<(Value, i64)> {
    let key = format!("{code}-{note}");
    let created = paths
        .route(
            "/widget/create",
            &json!([{"request_id": key, "id": uuid::Uuid::now_v7().to_string(), "idempotency_key": key, "code": code, "note": note}]),
        )
        .await?;
    Ok((value(&created)?["id"].clone(), edit_version(&created)?))
}

/// Update the note of a widget at `version`, and return the next version.
async fn note_widget(paths: &Paths, id: &Value, version: i64, note: &str) -> anyhow::Result<i64> {
    let updated = paths
        .route(
            "/widget/update",
            &json!([{
                "request_id": format!("note-{note}-{version}"), "id": id,
                "expected_edit_version": version,
                "change": {"note": note},
            }]),
        )
        .await?;
    edit_version(&updated)
}

/// The change feed of a list read (docs/architecture/execution.md#change-feed).
/// The fixture allows two widget codes, so the test works with two rows.
///
/// A feed read right after a list returns no rows, because every write came
/// before the list. Two writes after the list arrive with the statement snapshot. A
/// row changed three times appears once, with its latest data. An unchanged
/// feed keeps its cursor, and its weak tag answers 304. A row that leaves the
/// list's filter, and a deleted row, arrive as deleted with no data. A cursor
/// of another model is refused. After a real pruning run of the P30D
/// retention, the first cursor is older than the oldest entry kept, and the
/// feed refuses it with `cursor_expired`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN"]
async fn the_feed_of_a_list_read_follows_its_snapshot() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
        "WAMN_NATIVE_C_NATS_BIN",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let (application, paths) = start(system.url(), project.url(), &scratch, "admin").await?;
    let result = async {
        integer_identity_operations(&paths, project.url()).await?;
        let all = json!({});
        let (first, first_version) = create_widget(&paths, "standard", "kept").await?;
        let (second, second_version) = create_widget(&paths, "priority", "kept").await?;

        // FEED AFTER SNAPSHOT. The list returns the cursor of its snapshot,
        // and the writes before it do not appear.
        let page = paths.route("/widget/query", &json!([{}])).await?;
        let cursor = value(&page)?["cursor"]
            .as_str()
            .with_context(|| format!("a list page carries its feed cursor: {page}"))?
            .to_owned();
        for row in value(&page)?["item"].as_array().context("list rows")? {
            anyhow::ensure!(
                row["row_version"] == json!(feed_xmin(&cursor)?.to_string()),
                "list rows carry their statement xmin: {row}"
            );
        }
        let before = widget_changes(&paths, &all, &cursor).await?;
        anyhow::ensure!(
            before["rows"] == json!([]) && before["complete"] == json!(true),
            "the writes before the list do not appear: {before}"
        );
        let w1 = note_widget(&paths, &first, first_version, "w1").await?;
        let w2 = note_widget(&paths, &second, second_version, "w2").await?;
        let changes = widget_changes(&paths, &all, &cursor).await?;
        feed_versions(&changes)?;
        let one = change_of(&changes, &first)?;
        anyhow::ensure!(
            one["data"]["edit_version"] == json!(w1)
                && one["deleted"] == json!(false)
                && one["data"]["note"] == json!("w1"),
            "W1 arrives with its snapshot version and data: {one}"
        );
        anyhow::ensure!(
            change_of(&changes, &second)?["data"]["edit_version"] == json!(w2),
            "W2 arrives with its latest data: {changes}"
        );

        // FEED COLLAPSES. A row changed three times appears once, with its
        // latest data.
        let next = changes["next_cursor"]
            .as_str()
            .context("a next cursor")?
            .to_owned();
        let mut version = w1;
        for note in ["c1", "c2", "kept-c3"] {
            version = note_widget(&paths, &first, version, note).await?;
        }
        let collapsed = widget_changes(&paths, &all, &next).await?;
        feed_versions(&collapsed)?;
        let latest = change_of(&collapsed, &first)?;
        anyhow::ensure!(
            latest["data"]["edit_version"] == json!(version)
                && latest["data"]["note"] == json!("kept-c3"),
            "a row changed three times appears once, with its latest data: {collapsed}"
        );

        // UNCHANGED. A feed with nothing new returns its cursor unchanged, and
        // the same read with its tag answers 304.
        let idle = collapsed["next_cursor"]
            .as_str()
            .context("a next cursor")?
            .to_owned();
        let quiet = widget_changes(&paths, &all, &idle).await?;
        anyhow::ensure!(
            quiet["rows"] == json!([]) && quiet["next_cursor"] == json!(idle),
            "an unchanged feed keeps its cursor: {quiet}"
        );
        let feed_path = format!(
            "/widget/query?{}",
            wamn_execution_contract::encode_read_query(
                json!({"feed": {"cursor": idle}})
                    .as_object()
                    .context("an item")?
            )
        );
        let (status, tag, _) = paths.conditional(&feed_path, None).await?;
        let tag = tag
            .filter(|tag| status == 200 && tag.starts_with("W/\""))
            .with_context(|| format!("a feed read answers a weak ETag: {status}"))?;
        let (status, again, body) = paths.conditional(&feed_path, Some(&tag)).await?;
        anyhow::ensure!(
            status == 304 && again.as_deref() == Some(tag.as_str()) && body.is_empty(),
            "an unchanged feed answers 304: {status} {again:?} {body}"
        );

        // SCOPE. A row updated out of the list's filter arrives as deleted.
        let kept = json!({"note": ["kept"]});
        let scoped = paths
            .route("/widget/query", &json!([{"filter": kept}]))
            .await?;
        let scoped_cursor = value(&scoped)?["cursor"]
            .as_str()
            .context("a cursor")?
            .to_owned();
        note_widget(&paths, &first, version, "gone").await?;
        let scope_changes = widget_changes(&paths, &kept, &scoped_cursor).await?;
        feed_versions(&scope_changes)?;
        let gone = change_of(&scope_changes, &first)?;
        anyhow::ensure!(
            gone["deleted"] == json!(true) && gone.get("data").is_none(),
            "a row that left the filter arrives as deleted: {gone}"
        );

        // DELETE. A deleted row arrives as deleted, with no data and the
        // statement snapshot.
        let deleted_cursor = scope_changes["next_cursor"]
            .as_str()
            .context("a cursor")?
            .to_owned();
        value(
            &paths
                .route(
                    "/widget/delete",
                    &json!([{
                        "request_id": "feed-delete", "id": second,
                        "expected_edit_version": w2,
                    }]),
                )
                .await?,
        )?;
        let removed = widget_changes(&paths, &all, &deleted_cursor).await?;
        feed_versions(&removed)?;
        let removal = change_of(&removed, &second)?;
        anyhow::ensure!(
            removal["deleted"] == json!(true) && removal.get("data").is_none(),
            "a deleted row arrives as deleted with no data: {removal}"
        );

        // CURSOR OPAQUE. A cursor of another model, and one that does not
        // decode, are refused as invalid input.
        let foreign = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("widget_maker:1");
        for bad in [foreign.as_str(), "not-a-cursor"] {
            refused(
                &paths
                    .route("/widget/query", &json!([{"feed": {"cursor": bad}}]))
                    .await?,
                "invalid_input",
                &json!({"field": "cursor"}),
            )?;
        }

        // CURSOR EXPIRED. Every entry so far is older than the retention, and
        // a real pruning run removes them. One newer entry stays, so the first
        // cursor is older than the oldest entry kept.
        let (mut database, connection) =
            tokio_postgres::connect(project.url(), tokio_postgres::NoTls).await?;
        let connection = tokio::spawn(connection);
        database
            .execute(
                "UPDATE inventory.widget_history SET changed_at = now() - interval '31 days'",
                &[],
            )
            .await?;
        create_widget(&paths, "priority", "after").await?;
        let pruned = wamn_control::prune_record_history::prune(&mut database).await?;
        anyhow::ensure!(
            pruned
                .iter()
                .any(|history| history.history == "widget_history" && history.removed > 0),
            "the pruning run removes the backdated widget entries: {pruned:?}"
        );
        refused(
            &paths
                .route("/widget/query", &json!([{"feed": {"cursor": cursor}}]))
                .await?,
            "cursor_expired",
            &json!({}),
        )?;
        drop(database);
        connection.await??;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    application.shutdown().await?;
    result
}

/// A held transaction leaves room for one page of visible commits above xmin.
/// Replacing the priority row keeps the fixture's two-code constraint intact.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN"]
async fn the_feed_delivers_visible_rows_while_a_transaction_stays_open() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
        "WAMN_NATIVE_C_NATS_BIN",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let (application, paths) = start(system.url(), project.url(), &scratch, "admin").await?;
    let result = async {
        let (standard, version) = create_widget(&paths, "standard", "before").await?;
        let (mut writer, writer_connection) =
            tokio_postgres::connect(project.url(), tokio_postgres::NoTls)
                .await
                .context("connect the feed writer")?;
        let writer_connection = tokio::spawn(writer_connection);
        writer
            .batch_execute(
                "BEGIN;
                 SELECT set_config('app.user_id', (SELECT user_id::text FROM platform.user_role WHERE role_name = 'admin'), true),
                        set_config('app.operation', 'admin:feed-reader', true);
                 INSERT INTO platform.role (name)
                     SELECT 'reader' FROM platform.user_role WHERE role_name = 'admin';
                 INSERT INTO platform.permission (role_name, permission)
                     SELECT 'reader', 'platform-fixture:widget/query'
                     FROM platform.user_role WHERE role_name = 'admin';
                 UPDATE platform.user_role SET role_name = 'reader' WHERE role_name = 'admin';
                 COMMIT;",
            )
            .await
            .context("grant the list permission and move the caller to reader")?;
        let list = paths.route("/widget/query", &json!([{}])).await?;
        let cursor = value(&list)?["cursor"].as_str().context("a list cursor")?;
        let (mut holder, holder_connection) =
            tokio_postgres::connect(project.url(), tokio_postgres::NoTls)
                .await
                .context("connect the held writer")?;
        let holder_connection = tokio::spawn(holder_connection);
        let stamp = "SELECT set_config('app.user_id', (SELECT user_id::text FROM platform.user_role WHERE role_name = 'reader'), true),
                            set_config('app.operation', 'admin:feed-write', true)";
        let held = holder.transaction().await.context("begin the held transaction")?;
        held.batch_execute(stamp).await.context("stamp the held transaction")?;
        let held_row = held
            .query_one(
                "UPDATE inventory.widget SET note = 'held', edit_version = edit_version + 1
                 WHERE code = 'standard' RETURNING id::text, edit_version,
                 pg_current_xact_id()::text::int8",
                &[],
            )
            .await
            .context("update the held standard row")?;
        let xmin: i64 = held_row.get(2);
        let held_version = i64::from(held_row.get::<_, i32>(1));
        anyhow::ensure!(held_version == version + 1, "the held row advances its version");
        let mut expected = BTreeMap::from([(held_row.get::<_, String>(0), (held_version, false))]);
        let mut live_id = String::new();
        for version in 1_i32..=200 {
            let transaction = writer.transaction().await.with_context(|| format!("begin priority transaction {version}"))?;
            transaction.batch_execute(stamp).await.with_context(|| format!("stamp priority transaction {version}"))?;
            let deleted = transaction
                .execute("DELETE FROM inventory.widget WHERE code = 'priority'", &[])
                .await
                .with_context(|| format!("delete the prior priority row in transaction {version}"))?;
            anyhow::ensure!(deleted == u64::from(version > 1), "delete the previous id once");
            let row = transaction
                .query_one(
                    "INSERT INTO inventory.widget (code, note, edit_version)
                     VALUES ('priority', 'visible', $1) RETURNING id::text",
                    &[&version],
                )
                .await
                .with_context(|| format!("insert the priority row in transaction {version}"))?;
            live_id = row.get(0);
            anyhow::ensure!(
                expected.insert(live_id.clone(), (i64::from(version), version < 200)).is_none(),
                "each transaction inserts a new id"
            );
            transaction.commit().await.with_context(|| format!("commit priority transaction {version}"))?;
        }
        let snapshot_xmin: i64 = writer
            .query_one("SELECT pg_snapshot_xmin(pg_current_snapshot())::text::int8", &[])
            .await
            .context("read xmin while the standard row stays uncommitted")?
            .get(0);
        anyhow::ensure!(snapshot_xmin == xmin, "the held transaction holds xmin");
        let read = |cursor: &str| json!([{"feed": {"cursor": cursor, "limit": 100}}]);
        let first = paths.route("/widget/query", &read(cursor)).await?;
        let first = value(&first)?.clone();
        anyhow::ensure!(
            first["rows"].as_array().context("feed rows")?.len() == 100
                && first["complete"] == json!(true),
            "the first page fills with visible rows above xmin: {first}"
        );
        anyhow::ensure!(feed_versions(&first)? == Some(xmin), "visible changes use the held xmin");
        let next = first["next_cursor"].as_str().context("a next cursor")?;
        anyhow::ensure!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(next)?
                == format!("widget:{xmin}").as_bytes(),
            "the cursor is snapshot xmin without an id: {next}"
        );
        for row in first["rows"].as_array().context("feed rows")? {
            let id = row["id"].as_str().context("a change id")?;
            let transaction: i64 = writer
                .query_one(
                    "SELECT max(transaction_id) FROM inventory.widget_history WHERE row_key->>'id' = $1",
                    &[&id],
                )
                .await
                .with_context(|| format!("read the history transaction of {id}"))?
                .get(0);
            anyhow::ensure!(transaction > xmin, "{id} committed above xmin");
        }
        let repeated = paths.route("/widget/query", &read(next)).await?;
        anyhow::ensure!(value(&repeated)? == &first, "visible rows repeat while xmin is held");
        held.commit().await.context("commit the held standard row")?;

        let mut applied = BTreeMap::<String, i64>::new();
        let mut page = first;
        let mut empty = false;
        // One repeated page plus three pages can carry all 201 ids after commit.
        for _ in 0..5 {
            let snapshot = feed_versions(&page)?;
            let rows = page["rows"].as_array().context("feed rows")?;
            if rows.is_empty() {
                empty = true;
                break;
            }
            anyhow::ensure!(rows.len() <= 100, "pages stay bounded");
            for row in rows {
                let id = row["id"].as_str().context("a change id")?;
                let version = snapshot.context("a page snapshot")?;
                let &(final_version, deleted) = expected.get(id).context("an expected id")?;
                anyhow::ensure!(
                    version >= xmin && row["deleted"] == json!(deleted),
                    "{id} arrives with its snapshot and deletion: {row}"
                );
                if deleted {
                    anyhow::ensure!(row.get("data").is_none(), "a deleted row has no data");
                } else {
                    let (code, note) = if row["id"] == standard {
                        ("standard", "held")
                    } else {
                        anyhow::ensure!(id == live_id, "the last priority row stays live");
                        ("priority", "visible")
                    };
                    anyhow::ensure!(
                        row["data"]["id"] == row["id"]
                            && row["data"]["code"] == json!(code)
                            && row["data"]["note"] == json!(note)
                            && row["data"]["edit_version"] == json!(final_version),
                        "the live row carries its final data: {row}"
                    );
                }
                if applied.get(id).is_none_or(|previous| version >= *previous) {
                    applied.insert(id.to_owned(), version);
                }
            }
            let next = page["next_cursor"].as_str().context("a next cursor")?;
            let answer = paths.route("/widget/query", &read(next)).await?;
            page = value(&answer)?.clone();
        }
        anyhow::ensure!(empty, "the feed reaches an empty page after the held commit");
        anyhow::ensure!(applied.len() == 201, "every written id arrives: {}", applied.len());
        drop(holder);
        drop(writer);
        holder_connection.await.context("join the held connection")?.context("close the held connection")?;
        writer_connection.await.context("join the feed writer connection")?.context("close the feed writer connection")?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    application.shutdown().await?;
    result
}

/// A role without the widget list's permission is refused the feed exactly as
/// it is refused the list: the feed is the list read with its feed part.
///
/// A PAT route admits only a project admin, so the caller starts as `admin`
/// and then holds the application role `reader`, which has no permission row.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN"]
async fn a_role_without_the_list_permission_is_refused_the_feed() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
        "WAMN_NATIVE_C_NATS_BIN",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let (application, paths) = start(system.url(), project.url(), &scratch, "admin").await?;
    let result = async {
        let (database, connection) =
            tokio_postgres::connect(project.url(), tokio_postgres::NoTls).await?;
        let connection = tokio::spawn(connection);
        database
            .batch_execute(
                "BEGIN;
                 SELECT set_config('app.user_id', (SELECT user_id::text FROM platform.user_role WHERE role_name = 'admin'), true),
                        set_config('app.operation', 'admin:feed-reader', true);
                 INSERT INTO platform.role (name)
                     SELECT 'reader' FROM platform.user_role WHERE role_name = 'admin';
                 UPDATE platform.user_role SET role_name = 'reader' WHERE role_name = 'admin';
                 COMMIT;",
            )
            .await
            .context("move the caller to a role with no permission")?;
        let path = format!("{ROUTE_PREFIX}/widget/query");
        let (list, list_body) = paths.get(&path, &json!([{}])).await?;
        let (feed, feed_body) = paths
            .get(&path, &json!([{"feed": {"cursor": "d2lkZ2V0OjE"}}]))
            .await?;
        anyhow::ensure!(
            list == 403 && feed == 403 && list_body == feed_body,
            "the list and its feed are refused alike: {list} {list_body} / {feed} {feed_body}"
        );
        drop(database);
        connection.await??;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    application.shutdown().await?;
    result
}

/// Local assembly does not run env apply, so install its sequence defaults
/// under the administration role that owns configuration writes.
fn seed_number_sequences(database: &wamn_test_postgres::Database) -> anyhow::Result<()> {
    use wamn_control_provision::{CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope};
    let role = wamn_control_provision::workload_generation_role(
        WorkloadRoleFamily::Administration,
        WorkloadRoleScope::ProjectEnvironment {
            org: "acme",
            project: "platform",
            environment: "dev",
            database: database.name(),
        },
        CredentialGeneration::A,
    )?;
    let caller = database
        .execute(&["SELECT user_id::text FROM platform.user_role WHERE role_name = 'admin'"])?;
    database.execute(&[
        &wamn_control_provision::sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::Administration, database.name(), &role, "numbering-fixture", "2100-01-01T00:00:00Z",
        ),
        &format!("SET ROLE \"{role}\"; BEGIN; SET LOCAL app.user_id = '{}'; SET LOCAL app.operation = 'admin:seed-numbering';
            INSERT INTO platform.number_sequence (application, name, pattern) VALUES
            ('orders', 'label', 'L-{{seq:n}}'),
            ('orders', 'pick_label', 'B{{seq:batch,per=batch_id,width=4}}-{{seq:line,reset=batch,width=3}}'),
            ('orders', 'record_label', '{{field:stock_id}}-G{{seq:n,reset=stock_id,width=4}}'); COMMIT;", caller.trim()),
    ])?;
    Ok(())
}

/// Generated sequence bindings run in a guest command without any record.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN"]
async fn a_command_numbers_without_a_record_and_with_a_key_map() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
        "WAMN_NATIVE_C_NATS_BIN",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let (application, paths) = start(system.url(), project.url(), &scratch, "admin").await?;
    let result = async {
        seed_number_sequences(&project)?;
        for (index, batch, keyed) in [
            (1, "a", "B0001-001"),
            (2, "a", "B0001-002"),
            (3, "b", "B0002-001"),
        ] {
            let answer = paths
                .route(
                    "/number/issue",
                    &json!([{
                        "request_id": format!("number-{index}"),
                        "value": {"idempotency_key": format!("number-{index}"), "batch_id": batch},
                    }]),
                )
                .await?;
            assert_eq!(
                value(&answer)?,
                &json!({
                    "records": "0", "bare": format!("L-{index}"), "bare_peek": format!("L-{index}"),
                    "keyed": keyed, "keyed_peek": keyed,
                })
            );
        }
        Ok(())
    }
    .await;
    application.shutdown().await?;
    result
}

/// An overlay with no field declarations inherits the numbered insert and actors.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN"]
async fn an_overlay_insert_inherits_numbering_and_actor_stamps() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
        "WAMN_NATIVE_C_NATS_BIN",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/platform_fixture")
        .canonicalize()?;
    let overlay = base.with_file_name("platform_fixture_overlay");
    let components = PathBuf::from(std::env::var("WAMN_APPLICATION_COMPONENTS")?);
    let flow_http = PathBuf::from(std::env::var("WAMN_FLOW_HTTP_COMPONENT")?);
    let attachments = wamn_schema_generator::route_schema::read_package_attachments(&overlay)?;
    std::fs::write(
        scratch.path().join("environment.k"),
        include_str!("../fixtures/local-environments/platform.k").replace(
            "[\"platform_fixture\"]",
            "[\"platform_fixture\", \"platform_fixture_overlay\"]",
        ),
    )?;
    let application = LocalApplication::start(LocalApplicationConfig {
        system_database_url: system.url(),
        database_url: project.url(),
        scratch: scratch.path(),
        component_directory: &components,
        flow_http_wasm: &flow_http,
        env_key: "numbered-overlay",
        org: "acme",
        project: "platform",
        environment: "dev",
        schema: "inventory",
        caller_role: "admin",
        route_host: "platform.localhost",
        packages: &[
            LocalPackage {
                root: &base,
                component: "fixture",
            },
            LocalPackage {
                root: &overlay,
                component: "fixture_overlay",
            },
        ],
        attachments: &attachments,
    })
    .await?;
    let paths = Paths {
        endpoint: application.endpoint.clone(),
        host: application.route_host.clone(),
        bearer: application.bearer.clone(),
        client: reqwest::Client::new(),
    };
    let result = async {
        seed_number_sequences(&project)?;
        let caller = project.execute(&["SELECT user_id::text FROM platform.user_role WHERE role_name = 'admin'"])?;
        for (index, number) in [(1, "a-G0001"), (2, "a-G0002")] {
            let (status, answer) = paths.post("/overlay/numbered_label/create", &json!([{
                "request_id": format!("overlay-{index}"), "id": uuid::Uuid::now_v7().to_string(), "idempotency_key": format!("overlay-{index}"), "stock_id": "a",
            }])).await?;
            assert_eq!(status, 200, "{answer}");
            let row = value(&answer)?;
            assert_eq!(row["number"], number);
            assert_eq!(row["created_by"], caller.trim());
            assert_eq!(row["updated_by"], caller.trim());
        }
        Ok(())
    }.await;
    application.shutdown().await?;
    result
}

/// The generated integer operations use decimal strings across the HTTP and WIT boundaries.
async fn integer_identity_operations(paths: &Paths, database_url: &str) -> anyhow::Result<()> {
    let (database, connection) =
        tokio_postgres::connect(database_url, tokio_postgres::NoTls).await?;
    let connection = tokio::spawn(connection);
    // Custom sequence options are outside the introspected schema contract.
    // Seed the next generated value without changing that schema.
    database.query_one(
        "SELECT setval(pg_get_serial_sequence('inventory.counter', 'id'), 9007199254740993, false)",
        &[],
    ).await?;
    drop(database);
    connection.await??;

    let mut ids = Vec::new();
    for (index, expected) in ["9007199254740993", "9007199254740994"]
        .into_iter()
        .enumerate()
    {
        let created = paths.route("/counter/create", &json!([{
            "request_id": format!("counter-{index}"), "idempotency_key": format!("counter-{index}"),
            "label": format!("counter {index}"),
        }])).await?;
        let id = value(&created)?["id"].clone();
        anyhow::ensure!(
            id == json!(expected),
            "create returns the generated int8 as a string: {created}"
        );
        let read = paths.route("/counter/get", &json!([{"id": id}])).await?;
        anyhow::ensure!(
            value(&read)? == value(&created)?,
            "get returns the created record: {read}"
        );
        ids.push(id);
    }
    let page = paths
        .route("/counter/query", &json!([{"limit": 1}]))
        .await?;
    let first = value(&page)?;
    anyhow::ensure!(first["item"].as_array().is_some_and(|rows| rows.len() == 1));
    anyhow::ensure!(first["item"][0]["id"] == ids[0]);
    let next = first["next_cursor"]
        .as_str()
        .context("the first integer page has a cursor")?;
    let cursor_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(next)?;
    let cursor: Value = serde_json::from_slice(&cursor_bytes)?;
    anyhow::ensure!(
        cursor["id"] == ids[0],
        "the cursor preserves the full int8 string: {cursor}"
    );
    let page = paths
        .route("/counter/query", &json!([{"limit": 1, "cursor": next}]))
        .await?;
    let second = value(&page)?;
    anyhow::ensure!(
        second["item"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1)
    );
    anyhow::ensure!(second["item"][0]["id"] == ids[1] && second["next_cursor"].is_null());

    let updated = paths
        .route(
            "/counter/update",
            &json!([{
                "request_id": "counter-update", "id": ids[0], "expected_edit_version": 1,
                "change": {"label": "updated"},
            }]),
        )
        .await?;
    anyhow::ensure!(value(&updated)?["id"] == ids[0] && value(&updated)?["edit_version"] == 2);
    let read = paths
        .route("/counter/get", &json!([{"id": ids[0]}]))
        .await?;
    anyhow::ensure!(value(&read)?["label"] == "updated");
    value(
        &paths
            .route(
                "/counter/delete",
                &json!([{
                    "request_id": "counter-delete", "id": ids[1], "expected_edit_version": 1,
                }]),
            )
            .await?,
    )?;
    let missing = paths
        .route("/counter/get", &json!([{"id": ids[1]}]))
        .await?;
    refused(&missing, "not_found", &json!({"field": "id", "id": ids[1]}))?;

    let changes = paths
        .route(
            "/counter/query",
            &json!([{"feed": {"cursor": first["cursor"]}}]),
        )
        .await?;
    let changes = value(&changes)?;
    feed_versions(changes)?;
    let update = change_of(changes, &ids[0])?;
    anyhow::ensure!(
        update["deleted"] == false
            && update["data"]["id"] == ids[0]
            && update["data"]["label"] == "updated"
            && update["data"]["edit_version"] == 2
    );
    let delete = change_of(changes, &ids[1])?;
    anyhow::ensure!(delete["deleted"] == true && delete.get("data").is_none());
    anyhow::ensure!(changes["complete"] == true);
    Ok(())
}
