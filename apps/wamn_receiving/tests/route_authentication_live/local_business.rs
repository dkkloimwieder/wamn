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
        acme_inspection(&application, project.url()).await?;
        reads(&application).await
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

/// A write-log entry is keyed by the versionless operation, so a key stored
/// through Acme under one release answers the stored result under the next
/// (docs/plan/trx-pkg-plan.md, Idempotency ruling 2026-10-12). The local
/// runtime installs one release per database, so the second release serves
/// a second database that carries the first one's write-log row and none of
/// its business rows: a replay that ran again would find no purchase order.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT"]
async fn an_acme_key_replays_across_a_release() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
    ]);
    let _lock = wamn_test_postgres::lock();
    let scratch = ScratchRoot::create()?;
    for name in ["first", "second"] {
        fs::create_dir(scratch.path().join(name))?;
    }
    let connect = async |url: &str| -> anyhow::Result<tokio_postgres::Client> {
        let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls).await?;
        tokio::spawn(connection);
        Ok(client)
    };
    let release = async |client: &tokio_postgres::Client| -> anyhow::Result<String> {
        Ok(client
            .query_one("SELECT manifest_digest FROM catalog.releases", &[])
            .await?
            .get(0))
    };
    let key = "release-key";
    let item = async |application: &LocalApplication| {
        record(
            application,
            "/acme/receiving/record_receipt",
            key,
            key,
            "00000000-0000-0000-0000-000000000721",
            "00000000-0000-0000-0000-000000000821",
        )
        .await
    };

    let (first_system, first_project) = (
        wamn_test_postgres::database(),
        wamn_test_postgres::database(),
    );
    let first = start(
        first_system.url(),
        first_project.url(),
        &scratch.path().join("first"),
    )
    .await?;
    let first_client = connect(first_project.url()).await?;
    let stored = async {
        super::bind_fixture_principal(&first_client, super::identity().tenant.as_str()).await?;
        seed_acme_orders(&first_client).await?;
        item(&first).await
    }
    .await;
    first.shutdown().await?;
    let stored = stored?;
    ensure!(
        stored[0]["value"]["receipt_id"].is_string(),
        "the first release did not record the receipt: {stored}"
    );
    let claim = first_client
        .query_one(
            "SELECT request, result FROM app_system.write_log \
             WHERE operation = $1 AND idempotency_key = $2",
            &[&ACME_OPERATION, &key],
        )
        .await?;
    let (request, result): (Vec<u8>, Option<String>) = (claim.get(0), claim.get(1));

    let (second_system, second_project) = (
        wamn_test_postgres::database(),
        wamn_test_postgres::database(),
    );
    let second = start_with(
        second_system.url(),
        second_project.url(),
        &scratch.path().join("second"),
        &["client-acme-receiving:quality/approve-inspection"],
    )
    .await?;
    let second_client = connect(second_project.url()).await?;
    let replayed = async {
        ensure!(
            release(&second_client).await? != release(&first_client).await?,
            "the second start serves the first release"
        );
        second_client
            .execute(
                "INSERT INTO app_system.write_log (operation, idempotency_key, request, result) \
                 VALUES ($1, $2, $3, $4)",
                &[&ACME_OPERATION, &key, &request, &result],
            )
            .await?;
        item(&second).await
    }
    .await;
    second.shutdown().await?;
    let replayed = replayed?;
    ensure!(
        replayed[0]["value"] == stored[0]["value"],
        "the replay under the next release did not answer the stored result: {replayed}"
    );
    ensure!(
        second_client
            .query_one("SELECT NOT EXISTS (SELECT 1 FROM receiving.receipt)", &[])
            .await?
            .get::<_, bool>(0),
        "the replay under the next release ran again"
    );
    Ok(())
}

/// Serve Receiving and Acme on the local runtime with the record, read and
/// Acme routes, under a PAT caller that holds `admin`.
async fn start(
    system: &str,
    project: &str,
    scratch: &std::path::Path,
) -> anyhow::Result<LocalApplication> {
    start_with(system, project, scratch, &[]).await
}

/// [`start`], with the Acme routes of `more_overlay` served too, so the
/// release differs.
async fn start_with(
    system: &str,
    project: &str,
    scratch: &std::path::Path,
    more_overlay: &[&str],
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
        let operation =
            wamn_schema_generator::sealed_operation_reference(route_operation(&attachment));
        if matches!(
            operation,
            "client-acme-receiving:receiving/record-receipt"
                | "client-acme-receiving:quality/load-purchase-order-detail"
        ) || more_overlay.contains(&operation)
        {
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

/// Acme's inspection statement and one of Receiving's, by path.
const INSPECTION: &str = "command/record_receipt/record_inspection.sql";
const LOCK: &str = "command/record_receipt/lock_purchase_order.sql";

/// The versionless write-log operations of the two entries.
const ACME_OPERATION: &str = "client-acme-receiving:receiving/record-receipt";
const RECEIVING_OPERATION: &str = "wamn-receiving:receiving/record-receipt";

/// One record_receipt item on `path`, answered as JSON.
async fn record(
    application: &LocalApplication,
    path: &str,
    key: &str,
    reference: &str,
    order: &str,
    line: &str,
) -> anyhow::Result<Value> {
    let response = reqwest::Client::new()
        .post(format!("{}{path}", application.endpoint))
        .header(reqwest::header::HOST, &application.route_host)
        .bearer_auth(&application.bearer)
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
    project: &tokio_postgres::Client,
    key: &str,
) -> anyhow::Result<Vec<(String, Option<String>)>> {
    Ok(project
        .query(
            "SELECT operation, result FROM app_system.write_log \
             WHERE idempotency_key = $1 ORDER BY operation",
            &[&key],
        )
        .await?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect())
}

/// Acme's record_receipt calls Receiving's operation and records the
/// inspection in the same host transaction, under its own claim
/// (docs/plan/trx-pkg-plan.md, Spike and acceptance A: Real Acme behavior,
/// Idempotency, Authority).
async fn acme_inspection(application: &LocalApplication, database_url: &str) -> anyhow::Result<()> {
    let (project, connection) =
        tokio_postgres::connect(database_url, tokio_postgres::NoTls).await?;
    let connection = tokio::spawn(connection);
    super::bind_fixture_principal(&project, super::identity().tenant.as_str()).await?;
    seed_acme_orders(&project).await?;
    member_instances(
        &project,
        application
            .component_digests
            .get("receiving")
            .context("Receiving was not admitted")?,
    )
    .await?;

    // Not required: Receiving runs on its own named instance with only its
    // no-op participant plugged, the receipt commits, and no row is written.
    let none = record(
        application,
        "/acme/receiving/record_receipt",
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
        project
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM receiving.receipt WHERE id = $1::text::uuid) \
                 AND NOT EXISTS (SELECT 1 FROM receiving.quality_inspection WHERE receipt_id = $1::text::uuid)",
                &[&none_receipt],
            )
            .await?
            .get::<_, bool>(0),
        "inspection-not-required did not commit its receipt alone"
    );
    let none_claims = claims(&project, "part-none").await?;
    ensure!(
        matches!(
            none_claims.as_slice(),
            [(operation, Some(result))] if operation == ACME_OPERATION
                && serde_json::from_str::<Value>(result)?["value"]["receipt_id"] == none_receipt
        ),
        "the committed item did not leave one write-log entry under Acme's operation: {none_claims:?}"
    );

    // Approved: the inspection row is written. An unchanged key replays.
    let approved = record(
        application,
        "/acme/receiving/record_receipt",
        "part-approved",
        "part-approved",
        "00000000-0000-0000-0000-000000000721",
        "00000000-0000-0000-0000-000000000821",
    )
    .await?;
    let approved_receipt = approved[0]["value"]["receipt_id"]
        .as_str()
        .with_context(|| format!("the approved receipt failed: {approved}"))?;
    ensure!(project.query_one("SELECT status = 'approved' FROM receiving.quality_inspection WHERE receipt_id = $1::text::uuid", &[&approved_receipt]).await?.get::<_, bool>(0), "the approved receipt did not write an approved inspection");
    let replay = record(
        application,
        "/acme/receiving/record_receipt",
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
        project
            .query_one(
                "SELECT (SELECT count(*) = 1 FROM receiving.receipt WHERE purchase_order_id = '00000000-0000-0000-0000-000000000721') \
                        AND (SELECT count(*) = 1 FROM receiving.quality_inspection WHERE receipt_id = $1::text::uuid)",
                &[&approved_receipt],
            )
            .await?
            .get::<_, bool>(0),
        "a replay duplicated receipt or inspection state"
    );

    // Not approved: refused, and nothing commits, the receipt included.
    let refused = record(
        application,
        "/acme/receiving/record_receipt",
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
    ensure!(project.query_one("SELECT NOT EXISTS (SELECT 1 FROM receiving.receipt WHERE purchase_order_id = '00000000-0000-0000-0000-000000000722') AND (SELECT received_quantity = 0 FROM receiving.purchase_order_line WHERE id = '00000000-0000-0000-0000-000000000822')", &[]).await?.get::<_, bool>(0), "the refused inspection left Receiving's receipt behind");
    ensure!(
        claims(&project, "part-refused").await?.is_empty(),
        "the refused item left a claim"
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
        "/acme/receiving/record_receipt",
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
        project
            .query_one(
                "SELECT status = 'approved' FROM receiving.quality_inspection WHERE receipt_id = $1::text::uuid",
                &[&cross_receipt],
            )
            .await?
            .get::<_, bool>(0),
        "the key reused through Acme did not run Acme's inspection"
    );
    let cross_claims = claims(&project, "part-cross").await?;
    ensure!(
        cross_claims
            .iter()
            .map(|(operation, _)| operation.as_str())
            .collect::<Vec<_>>()
            == [ACME_OPERATION, RECEIVING_OPERATION],
        "one key through two entries is not two claims: {cross_claims:?}"
    );
    let receipts = project
        .query_one(
            "SELECT count(*) FROM receiving.receipt WHERE purchase_order_id = '00000000-0000-0000-0000-000000000723'",
            &[],
        )
        .await?
        .get::<_, i64>(0);
    eprintln!("cross-entry key part-cross: {receipts} receipt rows on its purchase order");

    // A key the participant path stored under Receiving's operation is not
    // Acme's claim, so a retry through Acme runs again.
    project
        .execute(
            "INSERT INTO app_system.write_log (operation, idempotency_key, request, result) \
             VALUES ($1, 'part-era', '\\x01'::bytea, '{\"request_id\":\"part-era\",\"value\":{}}')",
            &[&RECEIVING_OPERATION],
        )
        .await?;
    let era = record(
        application,
        "/acme/receiving/record_receipt",
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
            .post(format!("{}/acme/receiving/record_receipt", application.endpoint))
            .header(reqwest::header::HOST, &application.route_host)
            .bearer_auth(&application.bearer)
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
        project
            .query_one(
                "SELECT count(*) = 1 FROM receiving.receipt WHERE purchase_order_id = '00000000-0000-0000-0000-000000000725'",
                &[],
            )
            .await?
            .get::<_, bool>(0),
        "a reordered retry wrote a second receipt"
    );

    drop(project);
    connection
        .await
        .context("join the Acme database connection")??;
    Ok(())
}

/// The purchase orders of [`acme_inspection`]: 720 needs no inspection, 721
/// and 723 are approved, 722 is pending, 724 and 725 need none. 723 can take
/// two receipts.
async fn seed_acme_orders(project: &tokio_postgres::Client) -> anyhow::Result<()> {
    project
        .batch_execute(
            "INSERT INTO receiving.item (id, item_number) VALUES \
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
               ('00000000-0000-0000-0000-000000000826', '00000000-0000-0000-0000-000000000725', 2, '00000000-0000-0000-0000-000000000710', 1, 0);",
        )
        .await?;
    Ok(())
}

/// The served release binds Acme's composed component through one named
/// `statements` instance per member: Receiving's holds Receiving's
/// statements and not Acme's, and Acme's holds Acme's and no Receiving
/// statement, so a Receiving statement named through Acme's instance is
/// unknown to it (docs/plan/trx-pkg-plan.md, Per-member import instances).
async fn member_instances(
    project: &tokio_postgres::Client,
    receiving_digest: &str,
) -> anyhow::Result<()> {
    let bytes: Vec<u8> = project
        .query_one("SELECT canonical_bytes FROM catalog.releases", &[])
        .await?
        .get(0);
    let manifest: Value = serde_json::from_slice(&bytes)?;
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

/// The reads open their own guest transaction with begin and commit, and
/// run outside any host transaction, so the refusals of run, run-stream and
/// begin inside one leave them unchanged (docs/plan/trx-pkg-plan.md, Spike
/// and acceptance A, Reads unchanged).
async fn reads(application: &LocalApplication) -> anyhow::Result<()> {
    let http = reqwest::Client::new();
    let order = "00000000-0000-0000-0000-000000000720";
    for (path, item) in [
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
        let Value::Object(item) = item else {
            unreachable!("each read item is an object");
        };
        let query = wamn_execution_contract::encode_read_query(&item);
        let response = http
            .get(format!("{}{path}?{query}", application.endpoint))
            .header(reqwest::header::HOST, &application.route_host)
            .bearer_auth(&application.bearer)
            .send()
            .await?;
        let status = response.status();
        let body = response.json::<Value>().await?;
        ensure!(status.is_success(), "{path} returned {status}: {body}");
        ensure!(
            body.to_string().contains("\"rows\":[{"),
            "{path} answered no rows: {body}"
        );
    }
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
