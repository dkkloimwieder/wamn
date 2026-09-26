//! Receiving business histories over the local runtime and disposable PostgreSQL.

use std::fs;
use std::sync::Arc;

use anyhow::{Context as _, ensure};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};
use wamn_client::{ClientError, HttpRequest, HttpResponse, StaticPat, Transport, WamnClient};
use wamn_client_terminal::operator::{Action, Application};
use wamn_client_tui::screen::IntentValues;
use wamn_client_tui::submission::SessionBinding;
use wamn_integration_tests::local_application::{
    LocalApplication, LocalApplicationConfig, LocalPackage,
};
use wamn_receiving_tui::{Panel, ReceivingApplication};
use wamn_test_infrastructure::scratch::ScratchRoot;

#[derive(Debug)]
struct LocalTransport {
    http: reqwest::Client,
}

#[async_trait::async_trait]
impl Transport for LocalTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ClientError> {
        let mut outgoing = self.http.request(
            request.method.parse().map_err(|_| ClientError::Transport {
                detail: "local UI request has an invalid method".to_owned(),
            })?,
            &request.url,
        );
        for (name, value) in request.headers {
            outgoing = outgoing.header(name, value);
        }
        let response =
            outgoing
                .body(request.body)
                .send()
                .await
                .map_err(|error| ClientError::Transport {
                    detail: error.to_string(),
                })?;
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|error| ClientError::Transport {
                detail: error.to_string(),
            })?;
        Ok(HttpResponse {
            actor_labels: std::collections::BTreeMap::new(),
            status,
            body,
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT"]
async fn command_histories() -> anyhow::Result<()> {
    run_histories(false).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT"]
async fn formal_command_histories() -> anyhow::Result<()> {
    run_histories(true).await
}

async fn run_histories(formal: bool) -> anyhow::Result<()> {
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
        wamn_schema_generator::route_schema::read_package_attachments(&receiving)?;
    attachments.retain(|_, attachment| {
        matches!(
            route_operation(attachment),
            "wamn-receiving:purchase-order/query@1.0.0"
                | "wamn-receiving:purchase-order/update@1.0.0"
                | "wamn-receiving:receipt/get@1.0.0"
                | "wamn-receiving:receiving/record-receipt@1.0.0"
                | "wamn-receiving:receiving/load-receipt-screen@1.0.0"
                | "wamn-receiving:receiving/load-purchase-order-history@1.0.0"
                | "wamn-receiving:location/list@1.0.0"
        )
    });
    let mut packages = vec![LocalPackage {
        root: &receiving,
        component: "receiving",
        wirings: &[],
    }];
    if !formal {
        let overlay_attachments: std::collections::BTreeMap<
            String,
            wamn_catalog::ServingAttachment,
        > = wamn_schema_generator::route_schema::read_package_attachments(&acme)?;
        for (name, attachment) in overlay_attachments {
            if matches!(
                route_operation(&attachment),
                "client-acme-receiving:receiving/record-receipt@3.0.0"
                    | "client-acme-receiving:quality/load-purchase-order-detail@3.0.0"
            ) {
                attachments.insert(name, attachment);
            }
        }
        packages.push(LocalPackage {
            root: &acme,
            component: "client_acme_receiving",
            wirings: &[],
        });
    }
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
        packages: &packages,
    })
    .await?;
    let result = async {
        histories(
            &application,
            project.url(),
            &repository,
            scratch.path(),
            formal,
        )
        .await?;
        if !formal {
            transactional_participation(&application, project.url()).await?;
        }
        Ok(())
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
    let connection = tokio::spawn(connection);
    super::bind_fixture_principal(&project, super::TENANT).await?;
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
               ('00000000-0000-0000-0000-000000000733', 'SUPPLIER-733'); \
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

    let client = WamnClient::new(
        application.endpoint.clone(),
        Some(application.route_host.clone()),
        Arc::new(StaticPat::new(application.bearer.clone())?),
        Arc::new(LocalTransport { http: http.clone() }),
    );
    let mut ui = ReceivingApplication::acme(
        "Acme Receiving",
        SessionBinding {
            url: application.endpoint.clone(),
            host: Some(application.route_host.clone()),
            target_instance: "local-business".to_owned(),
        },
    );
    let action = ui.next_action();
    ui_dispatch(&mut ui, &client, action, "ui-orders").await?;
    ui_select(
        &mut ui,
        Panel::Orders,
        "id",
        "00000000-0000-0000-0000-000000000720",
    )?;
    ui_key(&mut ui, KeyCode::Enter);
    let action = ui.next_action();
    ui_dispatch(&mut ui, &client, action, "ui-lines").await?;
    let action = ui.next_action();
    ui_dispatch(&mut ui, &client, action, "ui-locations").await?;
    ui_select(
        &mut ui,
        Panel::Lines,
        "line_id",
        "00000000-0000-0000-0000-000000000820",
    )?;
    ui_key(&mut ui, KeyCode::Enter);
    ui_type(&mut ui, "1");
    ui_key(&mut ui, KeyCode::F(3));
    ui_type(&mut ui, "ui-part-none");
    ui_key(&mut ui, KeyCode::F(4));
    ui_select(
        &mut ui,
        Panel::Locations,
        "id",
        "00000000-0000-0000-0000-000000000711",
    )?;
    ui_key(&mut ui, KeyCode::Enter);
    let action = ui.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    ui_dispatch(&mut ui, &client, action, "part-none").await?;
    ensure!(
        ui.committed_result().is_some_and(|result| {
            result["purchase_order_id"] == "00000000-0000-0000-0000-000000000720"
                && result["purchase_order_status"] == "complete"
                && result["row_version"] == 2
                && result["receipt_id"].as_str().is_some()
        }),
        "the production UI did not show the committed receipt result"
    );
    let no_qc_receipt = project
        .query_one(
            "SELECT id::text FROM receiving.receipt WHERE idempotency_key = 'part-none'",
            &[],
        )
        .await?
        .get::<_, String>(0);
    ensure!(project.query_one("SELECT NOT EXISTS (SELECT 1 FROM receiving.quality_inspection WHERE receipt_id = $1::text::uuid)", &[&no_qc_receipt]).await?.get::<_, bool>(0), "inspection-not-required wrote quality control state");
    ui_key(&mut ui, KeyCode::Char('d'));
    let action = ui.next_action();
    ui_dispatch(&mut ui, &client, action, "ui-details").await?;
    ensure!(
        ui.panel() == Panel::Details
            && ui
                .screen(Panel::Details)
                .rows()
                .iter()
                .any(|row| row["id"] == "00000000-0000-0000-0000-000000000720"),
        "the production UI did not show the Acme purchase-order details"
    );

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

fn ui_key(application: &mut ReceivingApplication, code: KeyCode) -> Action {
    application.key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ui_intent(id: &str) -> IntentValues {
    IntentValues {
        request_id: id.to_owned(),
        idempotency_key: id.to_owned(),
        occurred_at: "2026-09-20T12:00:00.000000Z".to_owned(),
    }
}

async fn ui_dispatch(
    application: &mut ReceivingApplication,
    client: &WamnClient,
    action: Action,
    id: &str,
) -> anyhow::Result<()> {
    let request = application
        .prepare(action, Some(&ui_intent(id)))
        .map_err(|error| anyhow::anyhow!("prepare UI request {id}: {error}"))?;
    let response = if request.fresh_only {
        client
            .submit_fresh(&request.route, &request.parameters, &request.body)
            .await
    } else {
        client
            .submit(&request.route, &request.parameters, &request.body)
            .await
    };
    application.resolve(request.screen, request.attempt, response);
    Ok(())
}

fn ui_select(
    application: &mut ReceivingApplication,
    panel: Panel,
    field: &str,
    expected: &str,
) -> anyhow::Result<()> {
    let index = application
        .screen(panel)
        .rows()
        .iter()
        .position(|row| row[field] == expected)
        .with_context(|| format!("{panel:?} omitted {field}={expected}"))?;
    while application.selected_row(panel) < index {
        ui_key(application, KeyCode::Down);
    }
    while application.selected_row(panel) > index {
        ui_key(application, KeyCode::Up);
    }
    Ok(())
}

fn ui_type(application: &mut ReceivingApplication, value: &str) {
    for character in value.chars() {
        ui_key(application, KeyCode::Char(character));
    }
    ui_key(application, KeyCode::Enter);
}

async fn histories(
    application: &LocalApplication,
    database_url: &str,
    repository: &std::path::Path,
    evidence: &std::path::Path,
    formal: bool,
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
        "component_digests":application.component_digests,"formal":formal,
        "corpus_sha256":package["application_sql_corpus_identity"],"seed":7701,"cases":16,"history":null,
    }))?;
    let cancellation = pg_walstream::CancellationToken::new();
    let _cancel_on_exit = cancellation.clone().drop_guard();
    tokio::task::spawn_blocking(move || {
        super::command_histories::assert_histories_with_cancellation(&inputs, &cancellation)
    })
    .await
    .context("join the Receiving command histories")??;
    let evidence_rows = fs::read_to_string(path)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let summaries = evidence_rows
        .iter()
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
    if formal {
        let formal_summaries = evidence_rows
            .iter()
            .filter(|row| row["case"] == "formal-summary")
            .collect::<Vec<_>>();
        ensure!(
            formal_summaries.len() == 1
                && formal_summaries[0]["result"] == "pass"
                && formal_summaries[0]["generated_cases"] == 16
                && formal_summaries[0]["fixed_histories"]
                    .as_u64()
                    .is_some_and(|count| count > 0),
            "Receiving formal-model conformance results are absent or incomplete"
        );
    }
    Ok(())
}

fn route_operation(attachment: &wamn_catalog::ServingAttachment) -> &str {
    match &attachment.target {
        wamn_catalog::AttachmentTarget::Route { operation, .. } => operation,
        wamn_catalog::AttachmentTarget::Wiring { .. } => "",
    }
}
