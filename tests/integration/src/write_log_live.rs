//! The write log through the engine's intent rules and the platform fixture
//! component (`docs/history/host-transaction.md` 5, issue 5).
//!
//! One local application serves the fixture's routes over a disposable database. The
//! host claims each request element's key in the request element's transaction. The
//! create case posts `widget.create` request elements and reads `inventory.widget` and
//! `platform.write_log` directly: a retry with the same request answers the stored
//! result and adds no row, a changed request is refused, a refused request element
//! leaves its key free, and two concurrent calls with one key write one row.
//! The batch retry case now runs in the `application_routes_kind` product target.

use sqlx::Row as _;
use std::path::PathBuf;

use serde_json::{Value, json};
use wamn_test_infrastructure::scratch::ScratchRoot;

use crate::local_application::{LocalApplication, LocalApplicationConfig, LocalPackage};

#[path = "../tests/support/write_log_routes.rs"]
mod write_log_routes;
use write_log_routes::Routes;

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
    let attachments = wamn_compiler::publication::read_package_attachments(&app)?;
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
        project_env_key: "write-log",
        org: "acme",
        project: "fixture",
        environment: "dev",
        schema: "inventory",
        caller_role: "admin",
        route_host: "fixture.local.test",
        packages: &[LocalPackage {
            root: &app,
            component: "fixture",
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
async fn counts(database: &mut sqlx::PgConnection, key: &str) -> anyhow::Result<(i64, i64)> {
    let row = sqlx::query(
        "SELECT (SELECT count(*) FROM inventory.widget), \
                    (SELECT count(*) FROM platform.write_log \
                      WHERE operation = 'platform-fixture:widget/create' \
                        AND idempotency_key = $1)",
    )
    .bind(&key)
    .fetch_one(&mut *database)
    .await?;
    Ok((row.get(0), row.get(1)))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_APPLICATION_COMPONENTS, env:WAMN_FLOW_HTTP_COMPONENT, env:WAMN_NATIVE_C_NATS_BIN"]
async fn a_create_claims_its_key_in_the_write_log() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_APPLICATION_COMPONENTS, env:WAMN_FLOW_HTTP_COMPONENT, env:WAMN_NATIVE_C_NATS_BIN",
    );
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let (application, routes) = start(system.url(), project.url(), &scratch).await?;
    let mut database = wamn_postgres::connect(project.url()).await?;

    let first_id = uuid::Uuid::now_v7().to_string();
    let second_id = uuid::Uuid::now_v7().to_string();
    let third_id = uuid::Uuid::now_v7().to_string();

    // The first call writes one widget and one log row.
    let first = routes.create("r-1", "k-1", &first_id, "standard").await?;
    anyhow::ensure!(
        first.get("value").is_some(),
        "the first call creates: {first}"
    );
    anyhow::ensure!(counts(&mut database, "k-1").await? == (1, 1));

    // A retry with the same request answers the stored outcome and writes nothing.
    let retry = routes.create("r-2", "k-1", &first_id, "standard").await?;
    anyhow::ensure!(retry == first, "a retry answers {first}, not {retry}");
    anyhow::ensure!(counts(&mut database, "k-1").await? == (1, 1));

    let changed_id = routes.create("r-id", "k-1", &second_id, "standard").await?;
    anyhow::ensure!(changed_id["error"]["code"] == "idempotency_conflict");
    anyhow::ensure!(counts(&mut database, "k-1").await? == (1, 1));

    // The same key with another request is refused, and writes nothing.
    let changed = routes.create("r-3", "k-1", &first_id, "priority").await?;
    anyhow::ensure!(
        changed["error"]["code"] == "idempotency_conflict"
            && changed["error"]["detail"]
                == json!({"field": "idempotency_key", "intent": "platform-fixture:widget/create#k-1"}),
        "a changed request is refused: {changed}"
    );
    anyhow::ensure!(counts(&mut database, "k-1").await? == (1, 1));

    // A refused create leaves no log row, so a later different request under
    // the same key does its work.
    let refused = routes.create("r-4", "k-2", &second_id, "standard").await?;
    anyhow::ensure!(
        refused["error"]["code"] == "unique_violation",
        "a second standard widget is refused: {refused}"
    );
    anyhow::ensure!(counts(&mut database, "k-2").await? == (1, 0));
    let later = routes.create("r-5", "k-2", &second_id, "priority").await?;
    anyhow::ensure!(
        later.get("value").is_some(),
        "the key is free again: {later}"
    );
    anyhow::ensure!(counts(&mut database, "k-2").await? == (2, 1));

    // Two concurrent calls with one key write one row and give one answer. The
    // table's record history needs an actor for the cleanup.
    let actor = wamn_control_provision::PlatformComponent::Provisioning
        .principal_id()
        .to_string();
    sqlx::query(
        "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:write-log-test', false)",
    )
    .bind(&actor)
    .execute(&mut database)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::raw_sql("DELETE FROM inventory.widget")
        .execute(&mut database)
        .await
        .map(drop)?;
    let (left, right) = tokio::join!(
        routes.create("r-6", "k-3", &third_id, "standard"),
        routes.create("r-7", "k-3", &third_id, "standard"),
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
    anyhow::ensure!(counts(&mut database, "k-3").await? == (1, 1));

    // Replay keeps stored JSON bytes inside the response array. Only the
    // current request_id is inserted into the stored request element.
    let stored = " \n{ \"value\": {\"z\":1e+00, \"a\":-0.00} }\t";
    sqlx::query(
        "UPDATE platform.write_log SET result = $1 \
             WHERE operation = 'platform-fixture:widget/create' AND idempotency_key = 'k-3'",
    )
    .bind(&stored)
    .execute(&mut database)
    .await
    .map(|result| result.rows_affected())?;
    let response = routes
        .client
        .post(format!("{}/widget/create", routes.endpoint))
        .header("Host", &routes.host)
        .bearer_auth(&routes.bearer)
        .json(&json!([
            {"request_id": "r-8", "id": third_id, "idempotency_key": "k-3", "code": "standard"},
            {"request_id": "r-9", "id": first_id, "idempotency_key": "k-1", "code": "priority"},
        ]))
        .send()
        .await?;
    anyhow::ensure!(response.status() == reqwest::StatusCode::OK);
    let text = response.text().await?;
    anyhow::ensure!(
        text.starts_with("[ \n{ \"value\": {\"z\":1e+00, \"a\":-0.00} ,\"request_id\":\"r-8\"}\t,"),
        "replay preserves stored bytes in a mixed response: {text}"
    );
    let answer: Value = serde_json::from_str(&text)?;
    anyhow::ensure!(answer[1]["error"]["code"] == "idempotency_conflict");
    anyhow::ensure!(counts(&mut database, "k-3").await? == (1, 1));

    application.shutdown().await?;
    Ok(())
}
