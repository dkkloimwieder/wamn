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
                wirings: &[],
            },
        ],
    })
    .await?;
    let result = histories(&application, project.url(), &repository, scratch.path()).await;
    application.shutdown().await?;
    result
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
