//! Receiving business histories through the installed application.

use sqlx::Row as _;
#[path = "../../../../services/ctl/src/read_query.rs"]
mod read_query;

use std::fs;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_integration_tests::product_application::Application;
use wamn_test_infrastructure::scratch::ScratchRoot;

const INSPECTION: &str = "command/record_receipt/record_inspection.sql";
const LOCK: &str = "command/record_receipt/lock_purchase_order.sql";
const ACME_OPERATION: &str = "client-acme-receiving:acme/record-receipt";
const RECEIVING_OPERATION: &str = "wamn-receiving:receiving/record-receipt";

#[tokio::test(flavor = "multi_thread")]
async fn command_histories() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let scratch = ScratchRoot::create()?;
    let application = start().await?;
    let project_url = application.database_url().await?;
    let system_url = std::env::var("WAMN_SYSTEM_ADMIN_URL")?;
    let result = async {
        histories(&application, &system_url, &project_url, scratch.path()).await?;
        acme_inspection(&application, &project_url).await?;
        reads(&application).await
    }
    .await;
    application.remove().await?;
    result
}

/// Both screen reads return view lines, and closing an order removes its open lines.
#[tokio::test(flavor = "multi_thread")]
async fn receipt_screen_feeds_follow_open_orders() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let scratch = ScratchRoot::create()?;
    let application = start().await?;
    let project_url = application.database_url().await?;
    let system_url = std::env::var("WAMN_SYSTEM_ADMIN_URL")?;
    let result = async {
        let mut client = wamn_postgres::connect(&project_url).await?;

        super::bind_fixture_principal(&mut client).await?;
        seed_acme_orders(&mut client).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(("UPDATE receiving.purchase_order SET status = 'complete', row_version = row_version + 1
                WHERE id = '00000000-0000-0000-0000-000000000721';
             UPDATE receiving.purchase_order SET status = 'cancelled', row_version = row_version + 1
                WHERE id = '00000000-0000-0000-0000-000000000722';
             INSERT INTO receiving.purchase_order
                (id, purchase_order_number, supplier_id, status, acme_inspection_required, acme_quality_status)
             VALUES ('00000000-0000-0000-0000-000000000727', 'PO-FEED-EMPTY',
                '00000000-0000-0000-0000-000000000730', 'open', false, 'not_required');").to_string())).execute(&mut client).await.map(drop)?;
        let http = reqwest::Client::new();
        let read = async |path: &str, input: Value| -> anyhow::Result<Value> {
            let request_element = input.as_object().context("read input is an object")?;
            let query = read_query::encode_read_query(request_element);
            let response = http.get(format!("{}{path}?{query}", application.routes.endpoint))
                .header(reqwest::header::HOST, &application.routes.host)
                .bearer_auth(&application.routes.bearer).send().await?;
            let status = response.status();
            let body: Value = response.json().await?;
            ensure!(status.is_success(), "{path} returned {status}: {body}");
            let value = &body[0]["value"];
            ensure!(value.is_object(), "{path} refused the read: {body}");
            Ok(value.clone())
        };
        let screen_path = "/receiving/load_receipt_screen";
        for order in ["00000000-0000-0000-0000-000000000727", "00000000-0000-0000-0000-000000000999"] {
            let page = read(screen_path, json!({"purchase_order_id": order})).await?;
            ensure!(page["item"] == json!([]), "an empty or missing order returned lines: {page}");
            ensure!(page["cursor"].is_string());
        }
        let page = read(screen_path, json!({"purchase_order_id": "00000000-0000-0000-0000-000000000725"})).await?;
        let rows = page["item"].as_array().context("screen page rows")?;
        ensure!(rows.len() == 2 && rows[0]["line_number"] == 1 && rows[1]["line_number"] == 2);
        ensure!(rows.iter().all(|row| row["purchase_order_row_version"] == 1
            && row["row_version"].is_string() && row.as_object().unwrap().values().all(|value| !value.is_null())));
        let open_path = "/receiving/load_open_receipt_lines";
        let page = read(open_path, json!({})).await?;
        let rows = page["item"].as_array().context("open lines page rows")?;
        let ids = rows.iter().map(|row| row["line_id"].as_str().unwrap()).collect::<std::collections::BTreeSet<_>>();
        ensure!(ids == ["00000000-0000-0000-0000-000000000820", "00000000-0000-0000-0000-000000000823",
            "00000000-0000-0000-0000-000000000824", "00000000-0000-0000-0000-000000000825",
            "00000000-0000-0000-0000-000000000826"].into_iter().collect());
        ensure!(rows.iter().all(|row| row["purchase_order_status"] == "open"));
        let cursor = page["cursor"].as_str().context("open lines cursor")?;
        sqlx::query("UPDATE receiving.purchase_order SET status = 'complete', row_version = row_version + 1 WHERE id = '00000000-0000-0000-0000-000000000725'").execute(&mut client).await.map(|result| result.rows_affected())?;
        let changes = read(open_path, json!({"feed": {"cursor": cursor}})).await?;
        let rows = changes["rows"].as_array().context("open lines changes")?;
        ensure!(changes["complete"] == true && rows.len() == 2, "order close did not remove both lines: {changes}");
        let ids = rows.iter().map(|row| row["id"].as_str().unwrap()).collect::<std::collections::BTreeSet<_>>();
        ensure!(ids == ["00000000-0000-0000-0000-000000000825", "00000000-0000-0000-0000-000000000826"].into_iter().collect());
        ensure!(rows.iter().all(|row| row["deleted"] == true && row.get("data").is_none()));
        let page = read(open_path, json!({})).await?;
        ensure!(page["item"].as_array().context("remaining open lines")?.len() == 3);
        drop(client);
        Ok(())
    }.await;
    application.remove().await?;
    result
}

/// The ACL is the entry, on both sides
/// (docs/architecture/execution.md#routing-and-availability). A role that holds
/// only Acme's record_receipt runs it, although the entry calls Receiving's
/// record_receipt. The same role calling Receiving's record_receipt directly is
/// refused, because nothing derives that grant from the entry it holds.
#[tokio::test(flavor = "multi_thread")]
async fn an_entry_grant_runs_the_entry_and_grants_nothing_it_calls() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let scratch = ScratchRoot::create()?;
    let application = start().await?;
    let project_url = application.database_url().await?;
    let system_url = std::env::var("WAMN_SYSTEM_ADMIN_URL")?;
    let result = async {
        let mut client = wamn_postgres::connect(&project_url).await?;

        super::bind_fixture_principal(&mut client).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(("INSERT INTO receiving.item (id, item_number) VALUES \
                   ('00000000-0000-0000-0000-000000000910', 'ITEM-ENTRY-GRANT'); \
                 INSERT INTO receiving.location (id, location_code) VALUES \
                   ('00000000-0000-0000-0000-000000000911', 'ENTRY-GRANT'); \
                 INSERT INTO receiving.supplier (id, name) VALUES \
                   ('00000000-0000-0000-0000-000000000930', 'SUPPLIER-930'), \
                   ('00000000-0000-0000-0000-000000000931', 'SUPPLIER-931'); \
                 INSERT INTO receiving.purchase_order \
                   (id, purchase_order_number, supplier_id, status, row_version, \
                    acme_inspection_required, acme_quality_status) VALUES \
                   ('00000000-0000-0000-0000-000000000920', 'PO-ENTRY-A', \
                    '00000000-0000-0000-0000-000000000930', 'open', 1, false, 'not_required'), \
                   ('00000000-0000-0000-0000-000000000921', 'PO-ENTRY-R', \
                    '00000000-0000-0000-0000-000000000931', 'open', 1, false, 'not_required'); \
                 INSERT INTO receiving.purchase_order_line \
                   (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity) VALUES \
                   ('00000000-0000-0000-0000-000000000940', '00000000-0000-0000-0000-000000000920', 1, '00000000-0000-0000-0000-000000000910', 1, 0), \
                   ('00000000-0000-0000-0000-000000000941', '00000000-0000-0000-0000-000000000921', 1, '00000000-0000-0000-0000-000000000910', 1, 0);").to_string())).execute(&mut client).await.map(drop)?;
        // The caller keeps its project membership and loses the application
        // role admin for an authored role with one row: Acme's entry.
        sqlx::query("INSERT INTO platform.role (name) VALUES ('receipt-clerk')").execute(&mut client).await.map(|result| result.rows_affected())?;
        sqlx::query("INSERT INTO platform.permission (role_name, permission) \
                 VALUES ('receipt-clerk', 'client-acme-receiving:acme/record-receipt')").execute(&mut client).await.map(|result| result.rows_affected())?;
        ensure!(
            sqlx::query("UPDATE platform.user_role SET role_name = 'receipt-clerk' \
                     WHERE role_name = 'admin'").execute(&mut client).await.map(|result| result.rows_affected())?
                == 1,
            "the caller did not hold admin alone"
        );
        let http = reqwest::Client::new();
        let record = async |path: &str, key: &str, order: &str, line: &str| {
            http.post(format!("{}{path}", application.routes.endpoint))
                .header(reqwest::header::HOST, &application.routes.host)
                .bearer_auth(&application.routes.bearer)
                .json(&json!([{"request_id":key,"value":{
                    "idempotency_key":key,
                    "purchase_order_id":order,
                    "receipt_reference":key,
                    "occurred_at":"2026-10-14T12:00:00.000000Z",
                    "line":[{"purchase_order_line_id":line,"quantity":"1","location_id":"00000000-0000-0000-0000-000000000911"}]
                }}]))
                .send()
                .await
        };

        let entry = record(
            "/acme/acme/record_receipt",
            "entry-grant",
            "00000000-0000-0000-0000-000000000920",
            "00000000-0000-0000-0000-000000000940",
        )
        .await?;
        ensure!(
            entry.status().is_success(),
            "the entry grant was refused: {}",
            entry.status()
        );
        let entry: Value = entry.json().await?;
        ensure!(
            entry[0]["value"]["receipt_id"].is_string(),
            "the entry did not record its receipt: {entry}"
        );

        let direct = record(
            "/receiving/record_receipt",
            "direct-call",
            "00000000-0000-0000-0000-000000000921",
            "00000000-0000-0000-0000-000000000941",
        )
        .await?;
        ensure!(
            direct.status() == reqwest::StatusCode::FORBIDDEN,
            "the operation the entry calls was admitted directly: {}",
            direct.status()
        );
        let direct: Value = direct.json().await?;
        ensure!(
            direct["error"]["code"] == "permission-denied"
                && direct["error"]["operation"]
                    .as_str()
                    .is_some_and(|operation| operation
                        .starts_with("wamn-receiving:receiving/record-receipt@")),
            "the refusal does not name the base operation: {direct}"
        );
        ensure!(
            sqlx::query("SELECT NOT EXISTS (SELECT 1 FROM receiving.receipt \
                     WHERE purchase_order_id = '00000000-0000-0000-0000-000000000921')").fetch_one(&mut client)
                .await?
                .get::<bool, _>(0),
            "the refused call wrote a receipt"
        );
        drop(client);
        Ok(())
    }
    .await;
    application.remove().await?;
    result
}

/// A write-log entry is keyed by the versionless operation, so a key stored
/// through Acme under one release answers the stored result under the next
/// (docs/architecture/data-access.md#the-write-log). The local runtime installs
/// one release per database, so the second release serves a second database
/// that carries the first one's write-log row and none of its business rows: a
/// replay that ran again would find no purchase order.
#[tokio::test(flavor = "multi_thread")]
async fn an_acme_key_replays_across_a_release() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let scratch = ScratchRoot::create()?;
    for name in ["first", "second"] {
        fs::create_dir(scratch.path().join(name))?;
    }
    let connect = async |url: &str| -> anyhow::Result<sqlx::PgConnection> {
        let client = wamn_postgres::connect(url).await?;

        Ok(client)
    };
    let release = async |client: &mut sqlx::PgConnection| -> anyhow::Result<String> {
        Ok(sqlx::query("SELECT manifest_digest FROM catalog.releases")
            .fetch_one(&mut *client)
            .await?
            .get(0))
    };
    let key = "release-key";
    let request_element = async |application: &Application| {
        record(
            application,
            "/acme/acme/record_receipt",
            key,
            key,
            "00000000-0000-0000-0000-000000000721",
            "00000000-0000-0000-0000-000000000821",
        )
        .await
    };

    let first: Application = panic!("wamn-eulfr.12.9: no product write-log import across releases");
    let first_url = first.database_url().await?;
    let mut first_client = connect(&first_url).await?;
    let stored = async {
        super::bind_fixture_principal(&mut first_client).await?;
        seed_acme_orders(&mut first_client).await?;
        request_element(&first).await
    }
    .await;
    let stored = stored?;
    ensure!(
        stored[0]["value"]["receipt_id"].is_string(),
        "the first release did not record the receipt: {stored}"
    );
    let claim = sqlx::query(
        "SELECT request, result FROM platform.write_log \
             WHERE operation = $1 AND idempotency_key = $2",
    )
    .bind(&ACME_OPERATION)
    .bind(&key)
    .fetch_one(&mut first_client)
    .await?;
    let (request, result): (Vec<u8>, Option<String>) = (claim.get(0), claim.get(1));

    let second = start_with().await?;
    let second_url = second.database_url().await?;
    let mut second_client = connect(&second_url).await?;
    let replayed = async {
        ensure!(
            release(&mut second_client).await? != release(&mut first_client).await?,
            "the second start serves the first release"
        );
        sqlx::query(
            "INSERT INTO platform.write_log (operation, idempotency_key, request, result) \
                 VALUES ($1, $2, $3, $4)",
        )
        .bind(&ACME_OPERATION)
        .bind(&key)
        .bind(&request)
        .bind(&result)
        .execute(&mut second_client)
        .await
        .map(|result| result.rows_affected())?;
        request_element(&second).await
    }
    .await;
    let replayed = replayed?;
    ensure!(
        replayed[0]["value"] == stored[0]["value"],
        "the replay under the next release did not answer the stored result: {replayed}"
    );
    ensure!(
        sqlx::query("SELECT NOT EXISTS (SELECT 1 FROM receiving.receipt)")
            .fetch_one(&mut second_client)
            .await?
            .get::<bool, _>(0),
        "the replay under the next release ran again"
    );
    Ok(())
}

/// Serve Receiving and Acme on the local runtime with the record, read and
/// Acme routes, under a PAT caller that holds `admin`.
async fn start() -> anyhow::Result<Application> {
    panic!("wamn-eulfr.12.9: no product Receiving data import");
}

async fn start_with() -> anyhow::Result<Application> {
    panic!("wamn-eulfr.12.5: no product fixture route override");
}

async fn record(
    application: &Application,
    path: &str,
    key: &str,
    reference: &str,
    order: &str,
    line: &str,
) -> anyhow::Result<Value> {
    let response = reqwest::Client::new()
        .post(format!("{}{path}", application.routes.endpoint))
        .header(reqwest::header::HOST, &application.routes.host)
        .bearer_auth(&application.routes.bearer)
        .json(&json!([{"request_id":key,"value":{
            "idempotency_key":key,
            "purchase_order_id":order,
            "receipt_reference":reference,
            "occurred_at":"2026-09-20T12:00:00.000000Z",
            "line":[{"purchase_order_line_id":line,"quantity":"1","location_id":"00000000-0000-0000-0000-000000000711"}]
        }}]))
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "{path} returned {}",
        response.status()
    );
    response.json::<Value>().await.map_err(Into::into)
}

/// The write-log rows of `key`, as (operation, result).
async fn claims(
    project: &mut sqlx::PgConnection,
    key: &str,
) -> anyhow::Result<Vec<(String, Option<String>)>> {
    Ok(sqlx::query(
        "SELECT operation, result FROM platform.write_log \
             WHERE idempotency_key = $1 ORDER BY operation",
    )
    .bind(&key)
    .fetch_all(&mut *project)
    .await?
    .iter()
    .map(|row| (row.get(0), row.get(1)))
    .collect())
}

/// Acme's record_receipt calls Receiving's operation and records the inspection
/// in the same host transaction, under its own claim
/// (docs/architecture/data-access.md#inputs-queries-and-transactions).
async fn acme_inspection(application: &Application, database_url: &str) -> anyhow::Result<()> {
    let mut project = wamn_postgres::connect(database_url).await?;

    super::bind_fixture_principal(&mut project).await?;
    seed_acme_orders(&mut project).await?;
    member_instances(&mut project).await?;

    // Not required: Receiving runs on its own named instance, the receipt
    // commits, and no row is written.
    let none = record(
        application,
        "/acme/acme/record_receipt",
        "part-none",
        "part-none",
        "00000000-0000-0000-0000-000000000720",
        "00000000-0000-0000-0000-000000000820",
    )
    .await?;
    let none_receipt = none[0]["value"]["receipt_id"]
        .as_str()
        .with_context(|| format!("the receipt without inspection failed: {none}"))?;
    ensure!(
        sqlx::query("SELECT EXISTS (SELECT 1 FROM receiving.receipt WHERE id = $1::text::uuid) \
                 AND NOT EXISTS (SELECT 1 FROM receiving.quality_inspection WHERE receipt_id = $1::text::uuid)").bind(&none_receipt).fetch_one(&mut project)
            .await?
            .get::<bool, _>(0),
        "inspection-not-required did not commit its receipt alone"
    );
    let none_claims = claims(&mut project, "part-none").await?;
    ensure!(
        matches!(
            none_claims.as_slice(),
            [(operation, Some(result))] if operation == ACME_OPERATION
                && serde_json::from_str::<Value>(result)?["value"]["receipt_id"] == none_receipt
        ),
        "the committed request element did not leave one write-log entry under Acme's operation: {none_claims:?}"
    );

    // Approved: the inspection row is written. An unchanged key replays.
    let approved = record(
        application,
        "/acme/acme/record_receipt",
        "part-approved",
        "part-approved",
        "00000000-0000-0000-0000-000000000721",
        "00000000-0000-0000-0000-000000000821",
    )
    .await?;
    let approved_receipt = approved[0]["value"]["receipt_id"]
        .as_str()
        .with_context(|| format!("the approved receipt failed: {approved}"))?;
    ensure!(sqlx::query("SELECT status = 'approved' FROM receiving.quality_inspection WHERE receipt_id = $1::text::uuid").bind(&approved_receipt).fetch_one(&mut project).await?.get::<bool, _>(0), "the approved receipt did not write an approved inspection");
    let replay = record(
        application,
        "/acme/acme/record_receipt",
        "part-approved",
        "part-approved",
        "00000000-0000-0000-0000-000000000721",
        "00000000-0000-0000-0000-000000000821",
    )
    .await?;
    ensure!(
        replay[0]["value"] == approved[0]["value"],
        "a replay through Acme did not answer the stored result: {replay}"
    );
    ensure!(
        sqlx::query("SELECT (SELECT count(*) = 1 FROM receiving.receipt WHERE purchase_order_id = '00000000-0000-0000-0000-000000000721') \
                        AND (SELECT count(*) = 1 FROM receiving.quality_inspection WHERE receipt_id = $1::text::uuid)").bind(&approved_receipt).fetch_one(&mut project)
            .await?
            .get::<bool, _>(0),
        "a replay duplicated receipt or inspection state"
    );

    // Not approved: refused, and nothing commits, the receipt included.
    let refused = record(
        application,
        "/acme/acme/record_receipt",
        "part-refused",
        "part-refused",
        "00000000-0000-0000-0000-000000000722",
        "00000000-0000-0000-0000-000000000822",
    )
    .await?;
    ensure!(
        refused[0]["error"]["code"] == "invalid_input"
            && refused[0]["error"]["detail"]["field"] == "purchase_order_id",
        "an unapproved inspection was not refused: {refused}"
    );
    ensure!(sqlx::query("SELECT NOT EXISTS (SELECT 1 FROM receiving.receipt WHERE purchase_order_id = '00000000-0000-0000-0000-000000000722') AND (SELECT received_quantity = 0 FROM receiving.purchase_order_line WHERE id = '00000000-0000-0000-0000-000000000822')").fetch_one(&mut project).await?.get::<bool, _>(0), "the refused inspection left Receiving's receipt behind");
    ensure!(
        claims(&mut project, "part-refused").await?.is_empty(),
        "the refused request element left a claim"
    );

    // Cross-entry: one key through Receiving, then through Acme, is two
    // claims, and the second runs Acme's inspection.
    let direct = record(
        application,
        "/receiving/record_receipt",
        "part-cross",
        "part-cross-receiving",
        "00000000-0000-0000-0000-000000000723",
        "00000000-0000-0000-0000-000000000823",
    )
    .await?;
    ensure!(
        direct[0]["value"]["receipt_id"].is_string(),
        "the direct receipt failed: {direct}"
    );
    let cross = record(
        application,
        "/acme/acme/record_receipt",
        "part-cross",
        "part-cross-acme",
        "00000000-0000-0000-0000-000000000723",
        "00000000-0000-0000-0000-000000000823",
    )
    .await?;
    let cross_receipt = cross[0]["value"]["receipt_id"]
        .as_str()
        .with_context(|| format!("the key reused through Acme did not run: {cross}"))?;
    ensure!(
        sqlx::query("SELECT status = 'approved' FROM receiving.quality_inspection WHERE receipt_id = $1::text::uuid").bind(&cross_receipt).fetch_one(&mut project)
            .await?
            .get::<bool, _>(0),
        "the key reused through Acme did not run Acme's inspection"
    );
    let cross_claims = claims(&mut project, "part-cross").await?;
    ensure!(
        cross_claims
            .iter()
            .map(|(operation, _)| operation.as_str())
            .collect::<Vec<_>>()
            == [ACME_OPERATION, RECEIVING_OPERATION],
        "one key through two entries is not two claims: {cross_claims:?}"
    );
    let receipts = sqlx::query("SELECT count(*) FROM receiving.receipt WHERE purchase_order_id = '00000000-0000-0000-0000-000000000723'").fetch_one(&mut project)
        .await?
        .get::<i64, _>(0);
    eprintln!("cross-entry key part-cross: {receipts} receipt rows on its purchase order");

    // A key the participant path stored under Receiving's operation is not
    // Acme's claim, so a retry through Acme runs again.
    sqlx::query(
        "INSERT INTO platform.write_log (operation, idempotency_key, request, result) \
             VALUES ($1, 'part-era', '\\x01'::bytea, '{\"request_id\":\"part-era\",\"value\":{}}')",
    )
    .bind(&RECEIVING_OPERATION)
    .execute(&mut project)
    .await
    .map(|result| result.rows_affected())?;
    let era = record(
        application,
        "/acme/acme/record_receipt",
        "part-era",
        "part-era",
        "00000000-0000-0000-0000-000000000724",
        "00000000-0000-0000-0000-000000000824",
    )
    .await?;
    ensure!(
        era[0]["value"]["receipt_id"].is_string(),
        "a participant-era key did not run again through Acme: {era}"
    );

    // A retry through Acme with its lines in another order and its time
    // spelled another way is the same request under Acme's canonical form.
    let reordered = async |lines: [&str; 2], occurred_at: &str| -> anyhow::Result<Value> {
        let response = reqwest::Client::new()
            .post(format!("{}/acme/acme/record_receipt", application.routes.endpoint))
            .header(reqwest::header::HOST, &application.routes.host)
            .bearer_auth(&application.routes.bearer)
            .json(&json!([{"request_id":"part-reordered","value":{
                "idempotency_key":"part-reordered",
                "purchase_order_id":"00000000-0000-0000-0000-000000000725",
                "receipt_reference":"part-reordered",
                "occurred_at":occurred_at,
                "line":lines.map(|line| json!({"purchase_order_line_id":line,"quantity":"1","location_id":"00000000-0000-0000-0000-000000000711"}))
            }}]))
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "record_receipt returned {}",
            response.status()
        );
        response.json::<Value>().await.map_err(Into::into)
    };
    let first = reordered(
        [
            "00000000-0000-0000-0000-000000000825",
            "00000000-0000-0000-0000-000000000826",
        ],
        "2026-09-20T12:00:00.000000Z",
    )
    .await?;
    ensure!(
        first[0].get("value").is_some(),
        "the two-line receipt failed: {first}"
    );
    let retry = reordered(
        [
            "00000000-0000-0000-0000-000000000826",
            "00000000-0000-0000-0000-000000000825",
        ],
        "2026-09-20T12:00:00Z",
    )
    .await?;
    ensure!(
        retry[0]["value"] == first[0]["value"],
        "a reordered retry through Acme did not answer the stored result: {retry}"
    );
    ensure!(
        sqlx::query("SELECT count(*) = 1 FROM receiving.receipt WHERE purchase_order_id = '00000000-0000-0000-0000-000000000725'").fetch_one(&mut project)
            .await?
            .get::<bool, _>(0),
        "a reordered retry wrote a second receipt"
    );

    drop(project);

    Ok(())
}

/// The purchase orders of [`acme_inspection`]: 720 needs no inspection, 721
/// and 723 are approved, 722 is pending, 724 and 725 need none. 723 can take
/// two receipts.
async fn seed_acme_orders(project: &mut sqlx::PgConnection) -> anyhow::Result<()> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(("INSERT INTO receiving.item (id, item_number) VALUES \
               ('00000000-0000-0000-0000-000000000710', 'ITEM-ACME'); \
             INSERT INTO receiving.location (id, location_code) VALUES \
               ('00000000-0000-0000-0000-000000000711', 'ACME'); \
             INSERT INTO receiving.supplier (id, name) VALUES \
               ('00000000-0000-0000-0000-000000000730', 'SUPPLIER-730'), \
               ('00000000-0000-0000-0000-000000000731', 'SUPPLIER-731'), \
               ('00000000-0000-0000-0000-000000000732', 'SUPPLIER-732'), \
               ('00000000-0000-0000-0000-000000000733', 'SUPPLIER-733'), \
               ('00000000-0000-0000-0000-000000000734', 'SUPPLIER-734'), \
               ('00000000-0000-0000-0000-000000000735', 'SUPPLIER-735'); \
             INSERT INTO receiving.purchase_order \
               (id, purchase_order_number, supplier_id, status, row_version, \
                acme_inspection_required, acme_quality_status) VALUES \
               ('00000000-0000-0000-0000-000000000720', 'PO-ACME-0', \
                '00000000-0000-0000-0000-000000000730', 'open', 1, false, 'not_required'), \
               ('00000000-0000-0000-0000-000000000721', 'PO-ACME-A', \
                '00000000-0000-0000-0000-000000000731', 'open', 1, true, 'approved'), \
               ('00000000-0000-0000-0000-000000000722', 'PO-ACME-R', \
                '00000000-0000-0000-0000-000000000732', 'open', 1, true, 'pending'), \
               ('00000000-0000-0000-0000-000000000723', 'PO-ACME-X', \
                '00000000-0000-0000-0000-000000000733', 'open', 1, true, 'approved'), \
               ('00000000-0000-0000-0000-000000000724', 'PO-ACME-E', \
                '00000000-0000-0000-0000-000000000734', 'open', 1, false, 'not_required'), \
               ('00000000-0000-0000-0000-000000000725', 'PO-ACME-O', \
                '00000000-0000-0000-0000-000000000735', 'open', 1, false, 'not_required'); \
             INSERT INTO receiving.purchase_order_line \
               (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity) VALUES \
               ('00000000-0000-0000-0000-000000000820', '00000000-0000-0000-0000-000000000720', 1, '00000000-0000-0000-0000-000000000710', 1, 0), \
               ('00000000-0000-0000-0000-000000000821', '00000000-0000-0000-0000-000000000721', 1, '00000000-0000-0000-0000-000000000710', 1, 0), \
               ('00000000-0000-0000-0000-000000000822', '00000000-0000-0000-0000-000000000722', 1, '00000000-0000-0000-0000-000000000710', 1, 0), \
               ('00000000-0000-0000-0000-000000000823', '00000000-0000-0000-0000-000000000723', 1, '00000000-0000-0000-0000-000000000710', 2, 0), \
               ('00000000-0000-0000-0000-000000000824', '00000000-0000-0000-0000-000000000724', 1, '00000000-0000-0000-0000-000000000710', 1, 0), \
               ('00000000-0000-0000-0000-000000000825', '00000000-0000-0000-0000-000000000725', 1, '00000000-0000-0000-0000-000000000710', 1, 0), \
               ('00000000-0000-0000-0000-000000000826', '00000000-0000-0000-0000-000000000725', 2, '00000000-0000-0000-0000-000000000710', 1, 0);").to_string())).execute(&mut *project).await.map(drop)?;
    Ok(())
}

/// The served release binds Acme's composed component through one named
/// `statements` instance per member: Receiving's holds Receiving's statements
/// and not Acme's, and Acme's holds Acme's and no Receiving statement, so a
/// Receiving statement named through Acme's instance is unknown to it
/// (docs/architecture/components.md#grouping-and-publication).
async fn member_instances(project: &mut sqlx::PgConnection) -> anyhow::Result<()> {
    let bytes: Vec<u8> = sqlx::query("SELECT canonical_bytes FROM catalog.releases")
        .fetch_one(&mut *project)
        .await?
        .get(0);
    let manifest: Value = serde_json::from_slice(&bytes)?;
    let receiving_digest = manifest["components"]
        .as_array()
        .context("release components")?
        .iter()
        .find(|component| component["package-id"] == "wamn_receiving")
        .and_then(|component| component["descriptor"]["component-digest"].as_str())
        .context("Receiving component digest")?;
    let acme = manifest["components"]
        .as_array()
        .and_then(|components| {
            components
                .iter()
                .find(|component| component["package-id"] == "client_acme_receiving")
        })
        .context("the release serves no Acme component")?;
    let instances = acme["descriptor"]["statement-instances"]
        .as_object()
        .context("Acme's component has no named statements instances")?;
    let sets = acme["statement-instances"]
        .as_object()
        .context("the release binds no statement set to Acme's instances")?;
    let paths = |instance: &str| -> anyhow::Result<Vec<String>> {
        Ok(sets
            .get(instance)
            .and_then(Value::as_object)
            .with_context(|| format!("no statement set for {instance}"))?
            .keys()
            .cloned()
            .collect())
    };
    let mut receiving = None;
    let mut overlay = None;
    for (instance, digest) in instances {
        if digest.as_str() == Some(receiving_digest) {
            receiving = Some(paths(instance)?);
        } else {
            overlay = Some(paths(instance)?);
        }
    }
    let (Some(receiving), Some(overlay)) = (receiving, overlay) else {
        anyhow::bail!("Acme's component does not name one instance per member: {instances:?}");
    };
    ensure!(
        overlay.iter().any(|path| path == INSPECTION) && !overlay.iter().any(|path| path == LOCK),
        "Acme's instance does not hold exactly its own statements: {overlay:?}"
    );
    ensure!(
        receiving.iter().any(|path| path == LOCK)
            && !receiving.iter().any(|path| path == INSPECTION),
        "Receiving's instance does not hold exactly its own statements: {receiving:?}"
    );
    Ok(())
}

/// The reads open their own guest transaction with begin and commit, and run
/// outside any host transaction, so the refusals of run, run-stream and begin
/// inside one leave them unchanged
/// (docs/architecture/data-access.md#inputs-queries-and-transactions).
async fn reads(application: &Application) -> anyhow::Result<()> {
    let http = reqwest::Client::new();
    let order = "00000000-0000-0000-0000-000000000720";
    for (path, request_element) in [
        ("/location/list", json!({})),
        (
            "/receiving/load_receipt_screen",
            json!({"purchase_order_id": order}),
        ),
        (
            "/receiving/load_purchase_order_history",
            json!({"id": order, "limit": 10}),
        ),
    ] {
        let Value::Object(request_element) = request_element else {
            unreachable!("each read request element is an object");
        };
        let query = read_query::encode_read_query(&request_element);
        let response = http
            .get(format!("{}{path}?{query}", application.routes.endpoint))
            .header(reqwest::header::HOST, &application.routes.host)
            .bearer_auth(&application.routes.bearer)
            .send()
            .await?;
        let status = response.status();
        let body = response.json::<Value>().await?;
        ensure!(status.is_success(), "{path} returned {status}: {body}");
        ensure!(
            body[0]["value"]["rows"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
                || body[0]["value"]["item"]
                    .as_array()
                    .is_some_and(|rows| !rows.is_empty()),
            "{path} answered no rows: {body}"
        );
    }
    Ok(())
}

async fn histories(
    _application: &Application,
    _system_url: &str,
    _database_url: &str,
    evidence: &std::path::Path,
) -> anyhow::Result<()> {
    let path = evidence.join("receiving-correctness.jsonl");
    let inputs = panic!("wamn-eulfr.12.10: no product command-history fault fixture");
    let cancellation = pg_walstream::CancellationToken::new();
    let _cancel_on_exit = cancellation.clone().drop_guard();
    tokio::task::spawn_blocking(move || {
        super::command_histories::assert_histories_with_cancellation(&inputs, &cancellation)
    })
    .await
    .context("join the Receiving command histories")??;
    let summaries = fs::read_to_string(path)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|row| row["case"] == "summary")
        .collect::<Vec<_>>();
    ensure!(
        summaries.len() == 1
            && summaries[0]["result"] == "pass"
            && summaries[0]["generated_cases"] == 16
            && summaries[0]["boundary_cases"] == 7
            && summaries[0]["explicit_histories"]
                .as_u64()
                .is_some_and(|count| count > 0)
            && summaries[0]
                .get("reproduction")
                .is_none_or(|value| value == false),
        "Receiving command history results are absent or incomplete"
    );
    Ok(())
}
