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
    let repository = super::repository_root()?;
    let components = std::path::PathBuf::from(std::env::var("WAMN_APPLICATION_COMPONENTS")?);
    let flow_http = std::path::PathBuf::from(std::env::var("WAMN_FLOW_HTTP_COMPONENT")?);
    let receiving = repository.join("apps/wamn_receiving");
    let acme = repository.join("apps/client_acme_receiving");
    let mut attachments: std::collections::BTreeMap<String, wamn_catalog::ServingAttachment> =
        serde_json::from_slice(&fs::read(receiving.join("publication/attachments.json"))?)?;
    attachments.retain(|_, attachment| {
        matches!(
            attachment.wiring_id.as_str(),
            "purchase_order_update" | "receiving_record_receipt"
        )
    });
    let overlay_attachments: std::collections::BTreeMap<String, wamn_catalog::ServingAttachment> =
        serde_json::from_slice(&fs::read(acme.join("publication/attachments.json"))?)?;
    let overlay_receipt = overlay_attachments
        .into_iter()
        .find(|(_, attachment)| attachment.wiring_id == "receiving_record_receipt")
        .context("Acme publication omitted its record_receipt attachment")?;
    attachments.insert(overlay_receipt.0, overlay_receipt.1);
    let application = LocalApplication::start(LocalApplicationConfig {
        system_database_url: system.url(),
        database_url: project.url(),
        scratch: scratch.path(),
        component_directory: &components,
        flow_http_wasm: &flow_http,
        tenant: super::TENANT,
        org: super::ORG,
        project: super::PROJECT,
        environment: super::ENVIRONMENT,
        schema: "receiving",
        caller_role: "route-caller",
        route_host: "receiving.local.test",
        attachments: &attachments,
        packages: &[
            LocalPackage {
                root: &receiving,
                component: "receiving",
                wirings: &["purchase_order_update", "receiving_record_receipt"],
            },
            LocalPackage {
                root: &acme,
                component: "client_acme_receiving",
                wirings: &["receiving_record_receipt"],
            },
        ],
    })
    .await?;
    let result = async {
        histories(&application, project.url(), &repository, scratch.path()).await?;
        transactional_participation(&application, project.url()).await
    }
    .await;
    application.shutdown().await?;
    result
}

async fn transactional_participation(
    application: &LocalApplication,
    database_url: &str,
) -> anyhow::Result<()> {
    let (project, connection) =
        tokio_postgres::connect(database_url, tokio_postgres::NoTls).await?;
    let connection = tokio::spawn(async move { connection.await });
    super::bind_fixture_principal(&project, super::TENANT).await?;
    project
        .batch_execute(
            "INSERT INTO receiving.item (id, item_number) VALUES \
               ('00000000-0000-0000-0000-000000000710', 'ITEM-PARTICIPATION'); \
             INSERT INTO receiving.location (id, location_code) VALUES \
               ('00000000-0000-0000-0000-000000000711', 'PARTICIPATION'); \
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
                '00000000-0000-0000-0000-000000000733', 'open', 1, false, 'not_required'); \
             INSERT INTO receiving.purchase_order_line \
               (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity) VALUES \
               ('00000000-0000-0000-0000-000000000820', '00000000-0000-0000-0000-000000000720', 1, '00000000-0000-0000-0000-000000000710', 1, 0), \
               ('00000000-0000-0000-0000-000000000821', '00000000-0000-0000-0000-000000000721', 1, '00000000-0000-0000-0000-000000000710', 1, 0), \
               ('00000000-0000-0000-0000-000000000822', '00000000-0000-0000-0000-000000000722', 1, '00000000-0000-0000-0000-000000000710', 1, 0), \
               ('00000000-0000-0000-0000-000000000823', '00000000-0000-0000-0000-000000000723', 1, '00000000-0000-0000-0000-000000000710', 1, 0);",
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

    let no_qc = invoke(
        "/acme/receiving/record_receipt",
        "part-none",
        "00000000-0000-0000-0000-000000000720",
        "00000000-0000-0000-0000-000000000820",
    )
    .await?;
    let no_qc_receipt = no_qc[0]["value"]["receipt_id"]
        .as_str()
        .context("no-QC receipt failed")?;
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
                "SELECT (SELECT count(*) = 1 FROM receiving.receipt WHERE idempotency_key = 'part-approved') \
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

    drop(project);
    connection
        .await
        .context("join participation database connection")??;
    Ok(())
}

async fn histories(
    application: &LocalApplication,
    database_url: &str,
    repository: &std::path::Path,
    evidence: &std::path::Path,
) -> anyhow::Result<()> {
    let source = std::process::Command::new("git")
        .current_dir(repository)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()?;
    ensure!(
        source.status.success(),
        "read the Receiving source revision"
    );
    let package: Value = serde_json::from_slice(&fs::read(
        repository.join("apps/wamn_receiving/generated/package-weld.json"),
    )?)?;
    let path = evidence.join("receiving-correctness.jsonl");
    let inputs = serde_json::from_value(json!({
        "project_pg_url":database_url,"route_endpoint":application.endpoint,
        "route_host":application.route_host,"route_caller_secret":application.caller_secret_path,
        "tenant":super::TENANT,"caller_role":"route-caller","evidence_file":path,
        "source_commit":std::str::from_utf8(&source.stdout)?.trim(),
        "component_digests":application.component_digests,
        "corpus_sha256":package["application_sql_corpus_identity"],"seed":7701,"cases":16,"history":null,
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
