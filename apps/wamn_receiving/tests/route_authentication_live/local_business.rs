//! Receiving business histories over the local runtime and disposable PostgreSQL.

use std::fs;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_integration_tests::local_application::{
    LocalApplication, LocalApplicationConfig, LocalPackage,
};
use wamn_test_infrastructure::scratch::ScratchRoot;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT"]
async fn command_histories() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let application = start(system.url(), project.url(), scratch.path()).await?;
    let result = async {
        histories(&application, project.url(), scratch.path()).await?;
        transactional_participation(&application, project.url()).await
    }
    .await;
    application.shutdown().await?;
    result
}

/// The ACL is the entry, on both sides (docs/plan/trx-pkg-plan.md,
/// Authorization). A role that holds only Acme's record_receipt runs it,
/// although the entry calls Receiving's record_receipt. The same role calling
/// Receiving's record_receipt directly is refused, because nothing derives
/// that grant from the entry it holds.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT"]
async fn an_entry_grant_runs_the_entry_and_grants_nothing_it_calls() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let application = start(system.url(), project.url(), scratch.path()).await?;
    let result = async {
        let (client, connection) =
            tokio_postgres::connect(project.url(), tokio_postgres::NoTls).await?;
        let connection = tokio::spawn(connection);
        let tenant = super::identity().tenant.as_str();
        super::bind_fixture_principal(&client, tenant).await?;
        client
            .batch_execute(
                "INSERT INTO receiving.item (id, item_number) VALUES \
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
                   ('00000000-0000-0000-0000-000000000941', '00000000-0000-0000-0000-000000000921', 1, '00000000-0000-0000-0000-000000000910', 1, 0);",
            )
            .await?;
        // The caller keeps its project membership and loses the application
        // role admin for an authored role with one row: Acme's entry.
        client
            .execute(
                "INSERT INTO app_system.roles (tenant_id, name) VALUES ($1, 'receipt-clerk')",
                &[&tenant],
            )
            .await?;
        client
            .execute(
                "INSERT INTO app_system.permissions (tenant_id, role_name, permission) \
                 VALUES ($1, 'receipt-clerk', 'client-acme-receiving:receiving/record-receipt')",
                &[&tenant],
            )
            .await?;
        ensure!(
            client
                .execute(
                    "UPDATE app_system.user_roles SET role_name = 'receipt-clerk' \
                     WHERE tenant_id = $1 AND role_name = 'admin'",
                    &[&tenant],
                )
                .await?
                == 1,
            "the caller did not hold admin alone"
        );
        let http = reqwest::Client::new();
        let record = async |path: &str, key: &str, order: &str, line: &str| {
            http.post(format!("{}{path}", application.endpoint))
                .header(reqwest::header::HOST, &application.route_host)
                .bearer_auth(&application.bearer)
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
            "/acme/receiving/record_receipt",
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
            client
                .query_one(
                    "SELECT NOT EXISTS (SELECT 1 FROM receiving.receipt \
                     WHERE purchase_order_id = '00000000-0000-0000-0000-000000000921')",
                    &[],
                )
                .await?
                .get::<_, bool>(0),
            "the refused call wrote a receipt"
        );
        drop(client);
        connection.await??;
        Ok(())
    }
    .await;
    application.shutdown().await?;
    result
}

/// Serve Receiving and Acme on the local runtime with the record, read and
/// Acme routes, under a PAT caller that holds `admin`.
async fn start(
    system: &str,
    project: &str,
    scratch: &std::path::Path,
) -> anyhow::Result<LocalApplication> {
    let repository = super::repository_root()?;
    let components = std::path::PathBuf::from(std::env::var("WAMN_APPLICATION_COMPONENTS")?);
    let flow_http = std::path::PathBuf::from(std::env::var("WAMN_FLOW_HTTP_COMPONENT")?);
    let receiving = repository.join("apps/wamn_receiving");
    let acme = repository.join("apps/client_acme_receiving");
    let mut attachments: std::collections::BTreeMap<String, wamn_catalog::ServingAttachment> =
        wamn_schema_generator::route_schema::read_package_attachments(&receiving)?;
    attachments.retain(|_, attachment| {
        matches!(
            wamn_schema_generator::sealed_operation_reference(route_operation(attachment)),
            "wamn-receiving:purchase-order/query"
                | "wamn-receiving:purchase-order/update"
                | "wamn-receiving:receipt/get"
                | "wamn-receiving:receiving/record-receipt"
                | "wamn-receiving:receiving/load-receipt-screen"
                | "wamn-receiving:receiving/load-purchase-order-history"
                | "wamn-receiving:location/list"
        )
    });
    let overlay_attachments: std::collections::BTreeMap<String, wamn_catalog::ServingAttachment> =
        wamn_schema_generator::route_schema::read_package_attachments(&acme)?;
    for (name, attachment) in overlay_attachments {
        if matches!(
            wamn_schema_generator::sealed_operation_reference(route_operation(&attachment)),
            "client-acme-receiving:receiving/record-receipt"
                | "client-acme-receiving:quality/load-purchase-order-detail"
        ) {
            attachments.insert(name, attachment);
        }
    }
    LocalApplication::start(LocalApplicationConfig {
        system_database_url: system,
        database_url: project,
        scratch,
        component_directory: &components,
        flow_http_wasm: &flow_http,
        tenant: super::identity().tenant.as_str(),
        org: super::identity().org.as_str(),
        project: super::identity().project.as_str(),
        environment: super::identity().environment.as_str(),
        schema: "receiving",
        caller_role: "admin",
        route_host: "receiving.local.test",
        attachments: &attachments,
        packages: &[
            LocalPackage {
                root: &receiving,
                component: "receiving",
                wirings: &[],
            },
            LocalPackage {
                root: &acme,
                component: "client_acme_receiving",
                wirings: &[],
            },
        ],
    })
    .await
}

async fn transactional_participation(
    application: &LocalApplication,
    database_url: &str,
) -> anyhow::Result<()> {
    let (project, connection) =
        tokio_postgres::connect(database_url, tokio_postgres::NoTls).await?;
    let connection = tokio::spawn(connection);
    super::bind_fixture_principal(&project, super::identity().tenant.as_str()).await?;
    project
        .batch_execute(
            "INSERT INTO receiving.item (id, item_number) VALUES \
               ('00000000-0000-0000-0000-000000000710', 'ITEM-PARTICIPATION'); \
             INSERT INTO receiving.location (id, location_code) VALUES \
               ('00000000-0000-0000-0000-000000000711', 'PARTICIPATION'); \
             INSERT INTO receiving.supplier (id, name) VALUES \
               ('00000000-0000-0000-0000-000000000730', 'SUPPLIER-730'), \
               ('00000000-0000-0000-0000-000000000731', 'SUPPLIER-731'), \
               ('00000000-0000-0000-0000-000000000732', 'SUPPLIER-732'), \
               ('00000000-0000-0000-0000-000000000733', 'SUPPLIER-733'), \
               ('00000000-0000-0000-0000-000000000734', 'SUPPLIER-734'); \
             INSERT INTO receiving.purchase_order \
               (id, purchase_order_number, supplier_id, status, row_version, \
                acme_inspection_required, acme_quality_status) VALUES \
               ('00000000-0000-0000-0000-000000000720', 'PO-PART-0', \
                '00000000-0000-0000-0000-000000000730', 'open', 1, false, 'not_required'), \
               ('00000000-0000-0000-0000-000000000721', 'PO-PART-A', \
                '00000000-0000-0000-0000-000000000731', 'open', 1, true, 'approved'), \
               ('00000000-0000-0000-0000-000000000722', 'PO-PART-R', \
                '00000000-0000-0000-0000-000000000732', 'open', 1, true, 'pending'), \
               ('00000000-0000-0000-0000-000000000723', 'PO-PART-I', \
                '00000000-0000-0000-0000-000000000733', 'open', 1, false, 'not_required'), \
               ('00000000-0000-0000-0000-000000000724', 'PO-PART-O', \
                '00000000-0000-0000-0000-000000000734', 'open', 1, false, 'not_required'); \
             INSERT INTO receiving.purchase_order_line \
               (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity) VALUES \
               ('00000000-0000-0000-0000-000000000820', '00000000-0000-0000-0000-000000000720', 1, '00000000-0000-0000-0000-000000000710', 1, 0), \
               ('00000000-0000-0000-0000-000000000821', '00000000-0000-0000-0000-000000000721', 1, '00000000-0000-0000-0000-000000000710', 1, 0), \
               ('00000000-0000-0000-0000-000000000822', '00000000-0000-0000-0000-000000000722', 1, '00000000-0000-0000-0000-000000000710', 1, 0), \
               ('00000000-0000-0000-0000-000000000823', '00000000-0000-0000-0000-000000000723', 1, '00000000-0000-0000-0000-000000000710', 1, 0), \
               ('00000000-0000-0000-0000-000000000824', '00000000-0000-0000-0000-000000000724', 1, '00000000-0000-0000-0000-000000000710', 1, 0), \
               ('00000000-0000-0000-0000-000000000825', '00000000-0000-0000-0000-000000000724', 2, '00000000-0000-0000-0000-000000000710', 1, 0);",
        )
        .await?;

    let http = reqwest::Client::new();
    let invoke = |path: &'static str,
                  key: &'static str,
                  order: &'static str,
                  line: &'static str| {
        let http = http.clone();
        async move {
            let response = http
                .post(format!("{}{path}", application.endpoint))
                .header(reqwest::header::HOST, &application.route_host)
                .bearer_auth(&application.bearer)
                .json(&json!([{"request_id":key,"value":{
                    "idempotency_key":key,
                    "purchase_order_id":order,
                    "receipt_reference":key,
                    "occurred_at":"2026-09-20T12:00:00.000000Z",
                    "line":[{"purchase_order_line_id":line,"quantity":"1","location_id":"00000000-0000-0000-0000-000000000711"}]
                }}]))
                .send()
                .await?;
            ensure!(
                response.status().is_success(),
                "record_receipt returned {}",
                response.status()
            );
            response.json::<Value>().await.map_err(Into::into)
        }
    };

    let none = invoke(
        "/acme/receiving/record_receipt",
        "part-none",
        "00000000-0000-0000-0000-000000000720",
        "00000000-0000-0000-0000-000000000820",
    )
    .await?;
    ensure!(
        none[0].get("value").is_some(),
        "the receipt without inspection failed"
    );
    let no_qc_receipt = project
        .query_one(
            "SELECT id::text FROM receiving.receipt WHERE id = \
             (SELECT (result::jsonb -> 'value' ->> 'receipt_id')::uuid FROM app_system.write_log WHERE operation = 'wamn-receiving:receiving/record-receipt' AND idempotency_key = 'part-none')",
            &[],
        )
        .await?
        .get::<_, String>(0);
    ensure!(project.query_one("SELECT NOT EXISTS (SELECT 1 FROM receiving.quality_inspection WHERE receipt_id = $1::text::uuid)", &[&no_qc_receipt]).await?.get::<_, bool>(0), "inspection-not-required wrote quality control state");

    let approved = invoke(
        "/acme/receiving/record_receipt",
        "part-approved",
        "00000000-0000-0000-0000-000000000721",
        "00000000-0000-0000-0000-000000000821",
    )
    .await?;
    let approved_receipt = approved[0]["value"]["receipt_id"]
        .as_str()
        .context("approved receipt failed")?;
    ensure!(project.query_one("SELECT status = 'approved' FROM receiving.quality_inspection WHERE receipt_id = $1::text::uuid", &[&approved_receipt]).await?.get::<_, bool>(0), "approved participant did not write approved inspection");
    let replay = invoke(
        "/acme/receiving/record_receipt",
        "part-approved",
        "00000000-0000-0000-0000-000000000721",
        "00000000-0000-0000-0000-000000000821",
    )
    .await?;
    ensure!(
        replay[0]["value"] == approved[0]["value"],
        "extended replay changed its stored result"
    );
    ensure!(
        project
            .query_one(
                "SELECT (SELECT count(*) = 1 FROM receiving.receipt WHERE id = \
                         (SELECT (result::jsonb -> 'value' ->> 'receipt_id')::uuid FROM app_system.write_log WHERE operation = 'wamn-receiving:receiving/record-receipt' AND idempotency_key = 'part-approved')) \
                        AND (SELECT count(*) = 1 FROM receiving.quality_inspection WHERE receipt_id = $1::text::uuid)",
                &[&approved_receipt],
            )
            .await?
            .get::<_, bool>(0),
        "extended replay duplicated receipt or quality-control state"
    );

    let refused = invoke(
        "/acme/receiving/record_receipt",
        "part-refused",
        "00000000-0000-0000-0000-000000000722",
        "00000000-0000-0000-0000-000000000822",
    )
    .await?;
    ensure!(refused[0]["error"]["code"] == "invalid_input" && project.query_one("SELECT NOT EXISTS (SELECT 1 FROM receiving.receipt WHERE purchase_order_id = '00000000-0000-0000-0000-000000000722') AND (SELECT received_quantity = 0 FROM receiving.purchase_order_line WHERE id = '00000000-0000-0000-0000-000000000822')", &[]).await?.get::<_, bool>(0), "participant refusal did not roll back the joint transaction");

    let extended = invoke(
        "/acme/receiving/record_receipt",
        "part-intent",
        "00000000-0000-0000-0000-000000000723",
        "00000000-0000-0000-0000-000000000823",
    )
    .await?;
    ensure!(
        extended[0].get("value").is_some(),
        "extended intent command failed"
    );
    let changed = invoke(
        "/receiving/record_receipt",
        "part-intent",
        "00000000-0000-0000-0000-000000000723",
        "00000000-0000-0000-0000-000000000823",
    )
    .await?;
    ensure!(
        changed[0]["error"]["code"] == "idempotency_conflict",
        "direct command reused an extended intent identity"
    );

    // A retry through Acme with its lines in another order and its time
    // spelled another way is the same request under the base canonical form.
    let reordered = async |lines: [&str; 2], occurred_at: &str| -> anyhow::Result<Value> {
        let response = http
            .post(format!("{}/acme/receiving/record_receipt", application.endpoint))
            .header(reqwest::header::HOST, &application.route_host)
            .bearer_auth(&application.bearer)
            .json(&json!([{"request_id":"part-reordered","value":{
                "idempotency_key":"part-reordered",
                "purchase_order_id":"00000000-0000-0000-0000-000000000724",
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
            "00000000-0000-0000-0000-000000000824",
            "00000000-0000-0000-0000-000000000825",
        ],
        "2026-09-20T12:00:00.000000Z",
    )
    .await?;
    ensure!(
        first[0].get("value").is_some(),
        "the two-line receipt failed"
    );
    let retry = reordered(
        [
            "00000000-0000-0000-0000-000000000825",
            "00000000-0000-0000-0000-000000000824",
        ],
        "2026-09-20T12:00:00Z",
    )
    .await?;
    ensure!(
        retry[0]["value"] == first[0]["value"],
        "a reordered retry through Acme did not answer the stored result: {retry}"
    );
    ensure!(
        project
            .query_one(
                "SELECT count(*) = 1 FROM receiving.receipt WHERE purchase_order_id = '00000000-0000-0000-0000-000000000724'",
                &[],
            )
            .await?
            .get::<_, bool>(0),
        "a reordered retry wrote a second receipt"
    );

    drop(project);
    connection
        .await
        .context("join participation database connection")??;
    Ok(())
}

async fn histories(
    application: &LocalApplication,
    database_url: &str,
    evidence: &std::path::Path,
) -> anyhow::Result<()> {
    let path = evidence.join("receiving-correctness.jsonl");
    let inputs = serde_json::from_value(json!({
        "project_pg_url":database_url,"route_endpoint":application.endpoint,
        "route_host":application.route_host,"operator_secret":application.caller_secret_path,
        "tenant":super::identity().tenant.as_str(),"caller_role":"admin","evidence_file":path,
        "component_digests":application.component_digests,
        "packages":super::released_package_roots(),
        "record_receipt_operation":wamn_test_infrastructure::operations::sealed("wamn-receiving:receiving/record-receipt"),
        "seed":7701,"cases":16,"history":null,
    }))?;
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

fn route_operation(attachment: &wamn_catalog::ServingAttachment) -> &str {
    match &attachment.target {
        wamn_catalog::AttachmentTarget::Route { operation, .. } => operation,
        wamn_catalog::AttachmentTarget::Wiring { .. } => "",
    }
}
