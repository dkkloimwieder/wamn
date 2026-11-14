//! Route contracts through product deployment; SQL only observes results.

use anyhow::Context as _;
use serde_json::{Value, json};
use sqlx::Row as _;
#[path = "../../../services/ctl/src/read_query.rs"]
mod read_query;
use wamn_integration_tests::product_application::{Application, Routes};

#[path = "support/write_log.rs"]
mod write_log;

const PREREQUISITES: &str = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL";

/// A retry of `widget.record_batch` with its lines in another order and its
/// amounts spelled another way answers the stored result, because the host
/// applies the route's canonical form before it hashes the request element. A retry with
/// another amount is another request (`docs/history/host-transaction.md` 7,
/// ruling (a)).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL"]
async fn a_retry_with_reordered_lines_is_the_same_request() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(PREREQUISITES);
    let application = fixture("platform_fixture").await?;
    let routes = &application.routes;
    let mut database = application.observer().await?;

    let mut widgets = Vec::new();
    for (key, code) in [("w-1", "standard"), ("w-2", "priority")] {
        let created = routes
            .create(key, key, &uuid::Uuid::now_v7().to_string(), code)
            .await?;
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
    let logged: i64 = sqlx::query(
        "SELECT count(*) FROM platform.write_log \
              WHERE operation = 'platform-fixture:widget/record-batch' \
                AND idempotency_key = 'k-batch'",
    )
    .fetch_one(&mut database)
    .await?
    .get(0);
    anyhow::ensure!(logged == 1, "one key logs one row, not {logged}");

    let element = |request_id: &str, key: &str, amount: &str| {
        json!({
            "request_id": request_id,
            "value": {
                "idempotency_key": key,
                "note": null,
                "maker_id": null,
                "expected_edit_version": "1",
                "grade": "first",
                "inspector_id": null,
                "line": [{"widget_id": first, "amount": amount}],
            }
        })
    };
    let response = routes
        .client
        .post(format!("{}/widget/record_batch", routes.endpoint))
        .header("Host", &routes.host)
        .bearer_auth(&routes.bearer)
        .json(&json!([
            element("sibling-before", "sibling-before", "1"),
            element("refused-element", "refused-element", "-1"),
            element("sibling-after", "sibling-after", "2"),
        ]))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let answer: Value = response.json().await?;
    assert_eq!(answer.as_array().unwrap().len(), 3);
    for (index, request_id) in [(0, "sibling-before"), (2, "sibling-after")] {
        assert_eq!(answer[index]["request_id"], request_id);
        assert!(answer[index].get("value").is_some(), "{answer}");
    }
    assert_eq!(answer[1]["request_id"], "refused-element");
    assert_eq!(answer[1]["error"]["code"], "invalid_input");
    let committed: Vec<String> = sqlx::query(
        "SELECT idempotency_key FROM platform.write_log
         WHERE operation = 'platform-fixture:widget/record-batch'
         AND idempotency_key IN ('sibling-before', 'refused-element', 'sibling-after')
         ORDER BY idempotency_key",
    )
    .fetch_all(&mut database)
    .await?
    .into_iter()
    .map(|row| row.get(0))
    .collect();
    assert_eq!(committed, ["sibling-after", "sibling-before"]);

    application.remove().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL"]
async fn health_head_needs_no_credential() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(PREREQUISITES);
    let application = fixture("platform_fixture").await?;
    let paths = &application.routes;
    let response = paths
        .client
        .head(format!("{}/api/health", paths.endpoint))
        .header("Host", &paths.host)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(response.bytes().await?.is_empty());
    application.remove().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL"]
async fn a_read_of_100000_rows_refuses_at_the_ceiling() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(PREREQUISITES);
    let application = fixture("platform_fixture").await?;
    let paths = &application.routes;
    let load = json!([{"limit": 100_000}]);

    // The projection declares no limit, so the ceiling refuses the load.
    let list = "/widget_maker/list";
    let (status, answer) = get(paths, list, &load).await?;
    anyhow::ensure!(status == 200, "the refusal answers 200: {status} {answer}");
    anyhow::ensure!(
        answer[0]["error"]["code"] == "invalid_input"
            && answer[0]["error"]["detail"]
                == json!({"field": "limit", "minimum": "1", "maximum": "50000", "observed": "100000"}),
        "the request above the ceiling is refused: {answer}"
    );
    // A limit at the ceiling passes it, and is not the ceiling refusal.
    let (status, answer) = get(paths, list, &json!([{"limit": 50_000}])).await?;
    anyhow::ensure!(
        answer[0]["error"]["detail"]["maximum"] != json!("50000"),
        "a limit at the ceiling is not the ceiling refusal: {status} {answer}"
    );
    // The query's declared maximum refuses before the ceiling.
    let (status, answer) = get(paths, "/widget_maker/query", &load).await?;
    anyhow::ensure!(
        status == 400 && answer["error"]["data"]["pointer"] == json!("/0/limit"),
        "the query's maximum refuses a load of 100,000 rows: {status} {answer}"
    );

    application.remove().await?;
    Ok(())
}

async fn get(paths: &Routes, path: &str, body: &Value) -> anyhow::Result<(u16, Value)> {
    let input = body[0].as_object().context("read input")?;
    let query = read_query::encode_read_query(input);
    let response = paths
        .client
        .get(format!("{}{path}?{query}", paths.endpoint))
        .header("Host", &paths.host)
        .bearer_auth(&paths.bearer)
        .send()
        .await?;
    Ok((response.status().as_u16(), response.json().await?))
}

async fn fixture(package: &str) -> anyhow::Result<Application> {
    Application::prepare(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps")
            .join(package),
    )
    .await
}

/// Generated sequence bindings run in a guest command without any record.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL"]
async fn a_command_numbers_without_a_record_and_with_a_key_map() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(PREREQUISITES);
    let application = fixture("platform_fixture").await?;
    let paths = &application.routes;
    let result = async {
        for (index, batch, keyed) in [
            (1, "a", "B0001-001"),
            (2, "a", "B0001-002"),
            (3, "b", "B0002-001"),
        ] {
            let answer = post(
                paths,
                "/number/issue",
                &json!([{
                    "request_id": format!("number-{index}"),
                    "value": {"idempotency_key": format!("number-{index}"), "batch_id": batch},
                }]),
            )
            .await?;
            assert_eq!(
                &answer[0]["value"],
                &json!({
                    "records": "0", "bare": format!("L-{index}"), "bare_peek": format!("L-{index}"),
                    "keyed": keyed, "keyed_peek": keyed,
                })
            );
        }
        Ok(())
    }
    .await;
    application.remove().await?;
    result
}

/// An overlay with no field declarations inherits the numbered insert and actors.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL"]
async fn an_overlay_insert_inherits_numbering_and_actor_stamps() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(PREREQUISITES);
    let application = fixture("platform_fixture_overlay").await?;
    let caller = &application.caller;
    let paths = &application.routes;
    let result = async {
        for (index, number) in [(1, "a-G0001"), (2, "a-G0002")] {
            let answer = post(paths, "/overlay/numbered_label/create", &json!([{
                "request_id": format!("overlay-{index}"), "id": uuid::Uuid::now_v7().to_string(), "idempotency_key": format!("overlay-{index}"), "stock_id": "a",
            }])).await?;
            let row = &answer[0]["value"];
            assert_eq!(row["number"], number);
            assert_eq!(row["created_by"], caller.as_str());
            assert_eq!(row["updated_by"], caller.as_str());
        }
        Ok(())
    }.await;
    application.remove().await?;
    result
}

async fn post(paths: &Routes, path: &str, body: &Value) -> anyhow::Result<Value> {
    let response = paths
        .client
        .post(format!("{}{path}", paths.endpoint))
        .header("Host", &paths.host)
        .bearer_auth(&paths.bearer)
        .json(body)
        .send()
        .await?;
    let status = response.status();
    let answer = response.json().await?;
    anyhow::ensure!(
        status == reqwest::StatusCode::OK,
        "{path} answered {status}: {answer}"
    );
    Ok(answer)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL"]
async fn a_role_without_the_list_permission_is_refused_the_feed() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(PREREQUISITES);
    let application = fixture("platform_fixture").await?;
    let paths = &application.routes;
    let result = async {
        application
            .call("role.create", json!({"role":"reader"}))
            .await?;
        application
            .call(
                "user_role.grant",
                json!({"user_id":application.caller,"role":"reader"}),
            )
            .await?;
        application
            .call(
                "user_role.revoke",
                json!({"user_id":application.caller,"role":"admin"}),
            )
            .await?;
        let path = "/widget/query";
        let (list, list_body) = get(paths, path, &json!([{}])).await?;
        let (feed, feed_body) =
            get(paths, path, &json!([{"feed": {"cursor": "d2lkZ2V0OjE"}}])).await?;
        anyhow::ensure!(
            list == 403 && feed == 403 && list_body == feed_body,
            "the list and its feed are refused alike: {list} {list_body} / {feed} {feed_body}"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    application.remove().await?;
    result
}

/// A role without the authored read's permission is refused its feed exactly
/// as it is refused its page: one permission row protects both variants.
///
/// The caller loses `admin` before either request and holds `reader` alone.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL"]
async fn a_role_without_the_permission_is_refused_an_authored_feed() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(PREREQUISITES);
    let application = fixture("platform_fixture").await?;
    let paths = &application.routes;
    let result = async {
        application.call("role.create", json!({"role":"reader"})).await?;
        application.call("user_role.grant", json!({"user_id":application.caller,"role":"reader"})).await?;
        application.call("user_role.revoke", json!({"user_id":application.caller,"role":"admin"})).await?;
        let path = "/feed_line/screen";
        let (list, list_body) = get(paths, path, &json!([{}])).await?;
        let (feed, feed_body) = get(paths, path, &json!([{"feed": {"cursor": "ZmVlZF9saW5lLnNjcmVlbjox"}}]))
            .await?;
        anyhow::ensure!(
            list == 403 && feed == 403 && list_body == feed_body,
            "the authored read and its feed are refused alike: {list} {list_body} / {feed} {feed_body}"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    application.remove().await?;
    result
}
