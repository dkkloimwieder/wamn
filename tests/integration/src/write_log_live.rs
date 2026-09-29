//! The write log through the engine's intent rules and the platform fixture
//! component (`docs/plan/host-transaction.md` 5, issue 5).
//!
//! One local application serves the fixture's routes over a disposable
//! database. The host claims each item's key in the item's transaction. The
//! create case posts `widget.create` items and reads `inventory.widget` and
//! `app_system.write_log` directly: a retry with the same request answers the
//! stored result and adds no row, a changed request is refused, a refused
//! item leaves its key free, and two concurrent calls with one key write one
//! row. The batch case posts `widget.record_batch`: a retry with its lines in
//! another order and its amounts spelled another way is the same request.

use std::path::PathBuf;

use anyhow::Context as _;
use serde_json::{Value, json};
use tokio_postgres::NoTls;
use wamn_test_infrastructure::scratch::ScratchRoot;

use crate::local_application::{LocalApplication, LocalApplicationConfig, LocalPackage};

struct Routes {
    endpoint: String,
    host: String,
    bearer: String,
    client: reqwest::Client,
}

impl Routes {
    /// Post one `widget.create` item, and return its answer item.
    async fn create(&self, request_id: &str, key: &str, code: &str) -> anyhow::Result<Value> {
        self.post(
            "/widget/create",
            json!({"request_id": request_id, "idempotency_key": key, "code": code}),
        )
        .await
    }

    /// Post one `widget.record_batch` item with `lines` of `[widget_id,
    /// amount]`, and return its answer item.
    async fn record_batch(
        &self,
        request_id: &str,
        key: &str,
        lines: &[(&str, &str)],
    ) -> anyhow::Result<Value> {
        let line = lines
            .iter()
            .map(|(widget_id, amount)| json!({"widget_id": widget_id, "amount": amount}))
            .collect::<Vec<_>>();
        self.post(
            "/widget/record_batch",
            json!({"request_id": request_id, "value": {
                "idempotency_key": key,
                "note": null,
                "maker_id": null,
                "expected_edit_version": "1",
                "grade": "first",
                "inspector_id": null,
                "line": line,
            }}),
        )
        .await
    }

    /// Post one item to `path`, and return its answer item without its
    /// request id.
    async fn post(&self, path: &str, item: Value) -> anyhow::Result<Value> {
        let request_id = item["request_id"].clone();
        let response = self
            .client
            .post(format!("{}{path}", self.endpoint))
            .header("Host", &self.host)
            .bearer_auth(&self.bearer)
            .json(&json!([item]))
            .send()
            .await?;
        let status = response.status().as_u16();
        let text = response.text().await?;
        anyhow::ensure!(status == 200, "{path} answered {status}: {text}");
        let answer: Value = serde_json::from_str(&text)?;
        let mut item = answer[0].clone();
        anyhow::ensure!(
            item["request_id"] == request_id,
            "the answer keeps the request id: {answer}"
        );
        item.as_object_mut()
            .context("an answer item is an object")?
            .remove("request_id");
        Ok(item)
    }
}

async fn start(
    system_url: &str,
    project_url: &str,
    scratch: &ScratchRoot,
) -> anyhow::Result<(LocalApplication, Routes)> {
    let app = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/platform_fixture")
        .canonicalize()?;
    let components = PathBuf::from(std::env::var("WAMN_APPLICATION_COMPONENTS")?);
    let flow_http = PathBuf::from(std::env::var("WAMN_FLOW_HTTP_COMPONENT")?);
    let attachments = wamn_schema_generator::route_schema::read_package_attachments(&app)?;
    let application = LocalApplication::start(LocalApplicationConfig {
        system_database_url: system_url,
        database_url: project_url,
        scratch: scratch.path(),
        component_directory: &components,
        flow_http_wasm: &flow_http,
        tenant: "write-log",
        org: "acme",
        project: "fixture",
        environment: "dev",
        schema: "inventory",
        caller_role: "operator",
        route_host: "fixture.local.test",
        packages: &[LocalPackage {
            root: &app,
            component: "fixture",
            wirings: &[],
        }],
        attachments: &attachments,
    })
    .await?;
    let routes = Routes {
        endpoint: application.endpoint.clone(),
        host: application.route_host.clone(),
        bearer: application.bearer.clone(),
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?,
    };
    Ok((application, routes))
}

/// The rows of `inventory.widget`, and the log rows of `widget.create` by key.
async fn counts(database: &tokio_postgres::Client, key: &str) -> anyhow::Result<(i64, i64)> {
    let row = database
        .query_one(
            "SELECT (SELECT count(*) FROM inventory.widget), \
                    (SELECT count(*) FROM app_system.write_log \
                      WHERE operation = 'platform-fixture:widget/create' \
                        AND idempotency_key = $1)",
            &[&key],
        )
        .await?;
    Ok((row.get(0), row.get(1)))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT"]
async fn a_create_claims_its_key_in_the_write_log() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let (application, routes) = start(system.url(), project.url(), &scratch).await?;
    let (database, connection) = tokio_postgres::connect(project.url(), NoTls).await?;
    tokio::spawn(connection);

    // The first call writes one widget and one log row.
    let first = routes.create("r-1", "k-1", "standard").await?;
    anyhow::ensure!(
        first.get("value").is_some(),
        "the first call creates: {first}"
    );
    anyhow::ensure!(counts(&database, "k-1").await? == (1, 1));

    // A retry with the same request answers the stored outcome and writes nothing.
    let retry = routes.create("r-2", "k-1", "standard").await?;
    anyhow::ensure!(retry == first, "a retry answers {first}, not {retry}");
    anyhow::ensure!(counts(&database, "k-1").await? == (1, 1));

    // The same key with another request is refused, and writes nothing.
    let changed = routes.create("r-3", "k-1", "priority").await?;
    anyhow::ensure!(
        changed["error"]["code"] == "idempotency_conflict"
            && changed["error"]["detail"]
                == json!({"field": "idempotency_key", "intent": "platform-fixture:widget/create#k-1"}),
        "a changed request is refused: {changed}"
    );
    anyhow::ensure!(counts(&database, "k-1").await? == (1, 1));

    // A refused create leaves no log row, so a later different request under
    // the same key does its work.
    let refused = routes.create("r-4", "k-2", "standard").await?;
    anyhow::ensure!(
        refused["error"]["code"] == "unique_violation",
        "a second standard widget is refused: {refused}"
    );
    anyhow::ensure!(counts(&database, "k-2").await? == (1, 0));
    let later = routes.create("r-5", "k-2", "priority").await?;
    anyhow::ensure!(
        later.get("value").is_some(),
        "the key is free again: {later}"
    );
    anyhow::ensure!(counts(&database, "k-2").await? == (2, 1));

    // Two concurrent calls with one key write one row and give one answer. The
    // table's record history needs an actor for the cleanup.
    let actor = wamn_control_provision::PlatformComponent::Provisioning
        .principal_id()
        .to_string();
    database
        .execute(
            "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:write-log-test', false)",
            &[&actor],
        )
        .await?;
    database
        .batch_execute("DELETE FROM inventory.widget")
        .await?;
    let (left, right) = tokio::join!(
        routes.create("r-6", "k-3", "standard"),
        routes.create("r-7", "k-3", "standard"),
    );
    let (left, right) = (left?, right?);
    anyhow::ensure!(
        left.get("value").is_some(),
        "the first concurrent call creates: {left}"
    );
    anyhow::ensure!(
        left == right,
        "both calls answer one outcome: {left} and {right}"
    );
    anyhow::ensure!(counts(&database, "k-3").await? == (1, 1));

    application.shutdown().await?;
    Ok(())
}

/// A retry of `widget.record_batch` with its lines in another order and its
/// amounts spelled another way answers the stored result, because the host
/// applies the route's canonical form before it hashes the item. A retry with
/// another amount is another request (`docs/plan/host-transaction.md` 7,
/// ruling (a)).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT"]
async fn a_retry_with_reordered_lines_is_the_same_request() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let (application, routes) = start(system.url(), project.url(), &scratch).await?;
    let (database, connection) = tokio_postgres::connect(project.url(), NoTls).await?;
    tokio::spawn(connection);

    let mut widgets = Vec::new();
    for (key, code) in [("w-1", "standard"), ("w-2", "priority")] {
        let created = routes.create(key, key, code).await?;
        widgets.push(
            created["value"]["id"]
                .as_str()
                .with_context(|| format!("the create answers an id: {created}"))?
                .to_owned(),
        );
    }
    let (first, second) = (widgets[0].as_str(), widgets[1].as_str());

    let recorded = routes
        .record_batch("b-1", "k-batch", &[(second, "1.50"), (first, "2")])
        .await?;
    anyhow::ensure!(
        recorded.get("value").is_some(),
        "the first batch records: {recorded}"
    );
    let retry = routes
        .record_batch("b-2", "k-batch", &[(first, "02"), (second, "01.50")])
        .await?;
    anyhow::ensure!(
        retry == recorded,
        "a retry with reordered lines answers {recorded}, not {retry}"
    );
    let changed = routes
        .record_batch("b-3", "k-batch", &[(first, "3"), (second, "1.50")])
        .await?;
    anyhow::ensure!(
        changed["error"]["code"] == "idempotency_conflict"
            && changed["error"]["detail"]["field"] == "value.idempotency_key",
        "a changed amount is another request: {changed}"
    );
    let logged: i64 = database
        .query_one(
            "SELECT count(*) FROM app_system.write_log \
              WHERE operation = 'platform-fixture:widget/record-batch' \
                AND idempotency_key = 'k-batch'",
            &[],
        )
        .await?
        .get(0);
    anyhow::ensure!(logged == 1, "one key logs one row, not {logged}");

    application.shutdown().await?;
    Ok(())
}
