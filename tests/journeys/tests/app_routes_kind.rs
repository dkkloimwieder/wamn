//! Route contracts through product deployment; SQL only observes results.

mod common;

use anyhow::Context as _;
use common::{App, Routes};
use serde_json::{Value, json};
use sqlx::Row as _;

common::journey! {
    /// An identical retry of `widget.record_batch` answers the recorded result. A retry
    /// with its lines in another order and its amounts spelled another way is an
    /// idempotency conflict, because the hash keeps the supplied spelling and line order
    /// (`docs/architecture/data-access.md`).
    async fn a_retry_with_reordered_lines_is_an_idempotency_conflict() -> anyhow::Result<()> {
        let app = fixture(&"platform_fixture".parse().unwrap()).await?;
        let routes = &app.routes;
        let mut database = app.env.observe().await?;

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
            .record_batch("b-2", "k-batch", &[(second, "1.50"), (first, "2")])
            .await?;
        anyhow::ensure!(
            retry == recorded,
            "an identical retry answers {recorded}, not {retry}"
        );
        let reordered = routes
            .record_batch("b-3", "k-batch", &[(first, "02"), (second, "01.50")])
            .await?;
        anyhow::ensure!(
            reordered["error"]["code"] == "idempotency_conflict"
                && reordered["error"]["detail"]["field"] == "value.idempotency_key",
            "a reordered retry is an idempotency conflict: {reordered}"
        );
        let logged: i64 = sqlx::query(
            "SELECT count(*) FROM platform.write_log \
                  WHERE operation = 'platform-fixture:widget/record-batch' \
                    AND idempotency_key = 'k-batch'",
        )
        .fetch_one(&mut database.connection)
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
        assert_eq!(response.status(), hyper::StatusCode::OK);
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
        .fetch_all(&mut database.connection)
        .await?
        .into_iter()
        .map(|row| row.get(0))
        .collect();
        assert_eq!(committed, ["sibling-after", "sibling-before"]);

        Ok(())
    }
}

common::journey! {
    /// The entry refuses a query limit above the released maximum with an HTTP 400 pointer
    /// (`docs/architecture/execution.md`).
    async fn a_read_above_its_declared_maximum_is_refused() -> anyhow::Result<()> {
        let app = fixture(&"platform_fixture".parse().unwrap()).await?;
        let paths = &app.routes;
        let load = json!([{"limit": 100_000}]);
        let (status, answer) = get(paths, "/widget_maker/query", &load).await?;
        anyhow::ensure!(
            status == 400 && answer["error"]["data"]["pointer"] == json!("/0/limit"),
            "the query's maximum refuses a load of 100,000 rows: {status} {answer}"
        );

        Ok(())
    }
}

async fn get(paths: &Routes, path: &str, body: &Value) -> anyhow::Result<(u16, Value)> {
    let input = body[0].as_object().context("read input")?;
    let mut url = url::Url::parse(&format!("{}{path}", paths.endpoint))?;
    url.query_pairs_mut()
        .extend_pairs(input.iter().map(|(name, value)| (name, value.to_string())));
    let response = paths
        .client
        .get(url.as_str())
        .header("Host", &paths.host)
        .bearer_auth(&paths.bearer)
        .send()
        .await?;
    Ok((response.status().as_u16(), response.json().await?))
}

async fn fixture(package: &wamn_package::PackageName) -> anyhow::Result<App> {
    App::prepare(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps")
            .join(package.as_str()),
    )
    .await
}

common::journey! {
    /// Generated sequence bindings run in a guest command without any record.
    async fn a_command_numbers_without_a_record_and_with_a_key_map() -> anyhow::Result<()> {
        let app = fixture(&"platform_fixture".parse().unwrap()).await?;
        let paths = &app.routes;
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
            let bare = format!("L-{index}");
            let observed = &answer[0]["value"];
            anyhow::ensure!(
                observed["records"] == "0"
                    && observed["bare"] == bare
                    && observed["bare_peek"] == bare
                    && observed["keyed"] == keyed
                    && observed["keyed_peek"] == keyed,
                "number {index} answers {bare} and {keyed}: {answer}"
            );
        }
        Ok(())
    }
}

common::journey! {
    /// An overlay with no field declarations inherits the numbered insert and actors.
    async fn an_overlay_insert_inherits_numbering_and_actor_stamps() -> anyhow::Result<()> {
        let app = fixture(&"platform_fixture_overlay".parse().unwrap()).await?;
        let caller = app.caller().to_string();
        let paths = &app.routes;
        for (index, number) in [(1, "a-G0001"), (2, "a-G0002")] {
            let answer = post(paths, "/overlay/numbered_label/create", &json!([{
                "request_id": format!("overlay-{index}"), "id": uuid::Uuid::now_v7().to_string(), "idempotency_key": format!("overlay-{index}"), "stock_id": "a",
            }])).await?;
            let row = &answer[0]["value"];
            anyhow::ensure!(
                row["number"] == number && row["created_by"] == caller && row["updated_by"] == caller,
                "overlay {index} answers {number} stamped by {caller}: {answer}"
            );
        }
        Ok(())
    }
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
        status == hyper::StatusCode::OK,
        "{path} answered {status}: {answer}"
    );
    Ok(answer)
}

common::journey! {
    async fn a_role_without_the_list_permission_is_refused_the_feed() -> anyhow::Result<()> {
        let app = fixture(&"platform_fixture".parse().unwrap()).await?;
        let paths = &app.routes;
        let caller = app.caller().to_string();
        owner(&app, &["create-role", "--role", "reader"]).await?;
        owner(
            &app,
            &["grant-role", "--user-id", &caller, "--role", "reader"],
        )
        .await?;
        owner(
            &app,
            &[
                "revoke-role",
                "--user-id",
                &caller,
                "--role",
                "route-caller",
            ],
        )
        .await?;
        let path = "/widget/query";
        let (list, list_body) = get(paths, path, &json!([{}])).await?;
        let (feed, feed_body) = get(paths, path, &json!([{"feed": {"cursor": "d2lkZ2V0OjE"}}])).await?;
        anyhow::ensure!(
            list == 403 && feed == 403 && list_body == feed_body,
            "the list and its feed are refused alike: {list} {list_body} / {feed} {feed_body}"
        );
        Ok(())
    }
}

/// Run the role verb `args` on the application host with the org owner session, because
/// the role routes need `admin` and an API service cannot hold it.
async fn owner(app: &App, args: &[&str]) -> anyhow::Result<()> {
    let mut command = app.env.org.command();
    command
        .args(args)
        .args(["--host-url", &format!("http://{}", app.routes.host)]);
    let output = tokio::process::Command::from(command)
        .kill_on_drop(true)
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "wamn {args:?} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
