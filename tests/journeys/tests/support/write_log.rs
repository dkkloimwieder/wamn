//! The write log through the engine's intent rules and the platform fixture
//! component (`docs/history/host-transaction.md` 5, issue 5).
//!
//! A product deployment serves the fixture's routes. The
//! host claims each request element's key in the request element's transaction. The
//! create case posts `widget.create` request elements and reads `inventory.widget` and
//! `platform.write_log` directly: a retry with the same request answers the stored
//! result and adds no row, a changed request is refused, a refused request element
//! leaves its key free, and two concurrent calls with one key write one row.
//! The batch retry case now runs in the `application_routes_kind` product target.

use sqlx::Row as _;

use serde_json::{Value, json};

use wamn_test_application::Application;

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
async fn a_create_claims_its_key_in_the_write_log() -> anyhow::Result<()> {
    let application = Application::prepare(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/platform_fixture"),
    )
    .await?;
    let routes = &application.routes;
    let mut database = application.observer().await?;

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

    // Delete through the same command route used by clients before the concurrent create.
    for (id, created) in [(&first_id, &first), (&second_id, &later)] {
        let version = &created["value"]["edit_version"];
        let version = version
            .as_str()
            .map_or_else(|| version.to_string(), str::to_owned);
        let deleted = routes
            .post(
                "/widget/delete",
                json!({
                    "request_id":format!("delete-{id}"), "id":id, "expected_edit_version":version
                }),
            )
            .await?;
        anyhow::ensure!(
            deleted.get("value").is_some(),
            "widget delete refused: {deleted}"
        );
    }
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
    panic!("wamn-eulfr.12.9: no product stored-result import");
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

    application.remove().await?;
    Ok(())
}
