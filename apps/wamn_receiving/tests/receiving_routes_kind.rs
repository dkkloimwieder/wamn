//! Numbering and caller stamps through the product deployment and create routes.

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use sqlx::Row as _;
use std::path::Path;
use wamn_test_application::Application;

const PREREQUISITES: &str = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL"]
async fn a_purchase_order_create_takes_the_next_number_and_the_caller() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(PREREQUISITES);
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("Receiving source")?;
    let application = Application::prepare(source).await?;
    let result = async {
        let mut client = application.observer().await?;
        let http = reqwest::Client::new();
        let supplier = uuid::Uuid::now_v7().to_string();
        let created = http.post(format!("{}/supplier/create", application.routes.endpoint))
            .header(reqwest::header::HOST, &application.routes.host)
            .bearer_auth(&application.routes.bearer)
            .json(&json!([{"request_id":"supplier","id":supplier,"idempotency_key":"supplier","name":"SUPPLIER-950"}]))
            .send().await?;
        ensure!(created.status().is_success(), "supplier create refused: {}", created.status());
        let created: Value = created.json().await?;
        ensure!(created[0]["value"]["id"] == supplier, "supplier create refused: {created}");
        let mut numbers = Vec::new();
        for key in ["numbered-1", "numbered-2"] {
            let response = http
                .post(format!("{}/purchase_order/create", application.routes.endpoint))
                .header(reqwest::header::HOST, &application.routes.host)
                .bearer_auth(&application.routes.bearer)
                .json(
                    &json!([{"request_id": key, "id": uuid::Uuid::now_v7().to_string(), "idempotency_key": key, "supplier_id": supplier}]),
                )
                .send()
                .await?;
            ensure!(
                response.status().is_success(),
                "the create was refused: {}",
                response.status()
            );
            let body: Value = response.json().await?;
            let number = body[0]["value"]["purchase_order_number"]
                .as_str()
                .with_context(|| format!("the create answered no number: {body}"))?;
            numbers.push(number.to_owned());
        }
        ensure!(
            numbers == ["PO-000001", "PO-000002"],
            "the creates were numbered {numbers:?}"
        );
        let caller = &application.caller;
        let rows = sqlx::query("SELECT purchase_order_number, created_by::text, updated_by::text \
                 FROM receiving.purchase_order ORDER BY purchase_order_number").fetch_all(&mut client)
            .await?;
        let actors = rows
            .iter()
            .map(|row| {
                (
                    row.get::<String, _>(0),
                    row.get::<String, _>(1),
                    row.get::<String, _>(2),
                )
            })
            .collect::<Vec<_>>();
        ensure!(
            actors
                == numbers
                    .iter()
                    .map(|number| (number.clone(), caller.to_owned(), caller.to_owned()))
                    .collect::<Vec<_>>(),
            "the orders do not name the caller {caller}: {actors:?}"
        );
        for (field, value) in [
            ("created_by", caller.as_str()),
            ("purchase_order_number", "FORGED"),
        ] {
            let response = http
                .post(format!("{}/purchase_order/create", application.routes.endpoint))
                .header(reqwest::header::HOST, &application.routes.host)
                .bearer_auth(&application.routes.bearer)
                .json(&json!([{
                    "request_id": format!("supplied-{field}"),
                    "id": uuid::Uuid::now_v7().to_string(),
                    "idempotency_key": format!("supplied-{field}"),
                    "supplier_id": supplier, field: value,
                }]))
                .send()
                .await?;
            let status = response.status();
            let body: Value = response.json().await?;
            ensure!(
                status == reqwest::StatusCode::OK
                    && body[0]["error"]
                        == json!({"code": "invalid_input", "detail": {"field": "input"}}),
                "the route accepted caller-supplied {field}: {status} {body}"
            );
        }
        let count: i64 = sqlx::query("SELECT count(*) FROM receiving.purchase_order").fetch_one(&mut client)
            .await?
            .get(0);
        ensure!(count == 2, "a refused create wrote an order");
        drop(client);
        Ok(())
    }
    .await;
    application.remove().await?;
    result
}

mod development_assertions;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_DEV_ENV, env:WAMN_BROWSER_URL, env:WAMN_CTL_BIN, env:WAMN_SYSTEM_ADMIN_URL"]
async fn product_dev_command_applies_packages_and_cleans_up() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(PREREQUISITES);
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("Receiving source")?;
    let application = Application::prepare(source).await?;
    let mut system = wamn_postgres::connect(&std::env::var("WAMN_SYSTEM_ADMIN_URL")?).await?;
    sqlx::raw_sql("SET default_transaction_read_only = on")
        .execute(&mut system)
        .await
        .map(drop)?;
    let mut project = application.observer().await?;
    let before = development_state(&mut system, &mut project).await?;
    application.rebuild(source).await?;
    application.rebuild(source).await?;
    let after = development_state(&mut system, &mut project).await?;
    ensure!(
        before == after,
        "development changed PAT state or database ACLs"
    );
    let document = application.target();
    let key = wamn_registry::project_env_key(&document.org, &document.project, &document.env);
    development_assertions::verify_dev_target_package_and_acl_state(
        &mut project,
        &[source.to_owned()],
        &key,
    )
    .await?;
    application.remove().await
}

async fn development_state(
    system: &mut sqlx::PgConnection,
    project: &mut sqlx::PgConnection,
) -> anyhow::Result<(i64, Option<String>, Option<String>)> {
    let pats: i64 = sqlx::query("SELECT count(*) FROM identity.pats")
        .fetch_one(&mut *system)
        .await?
        .get(0);
    let acl = "SELECT (SELECT array_agg(entry::text ORDER BY entry::text) FROM pg_database, unnest(datacl) AS entry WHERE datname=current_database())::text";
    let system_acl = sqlx::query(acl).fetch_one(&mut *system).await?.get(0);
    let project_acl = sqlx::query(acl).fetch_one(&mut *project).await?.get(0);
    Ok((pats, system_acl, project_acl))
}
