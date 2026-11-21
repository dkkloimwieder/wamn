//! The workflow behaviour that exists today, through `wamn workflow author|enable|run`.
//!
//! The graphs are written by hand and call the application's own exports: the cloud
//! deploy offers no palette sibling, so no graph maps an entry's output (Epic 15 C16, G3).

mod common;

use std::time::Duration;

use anyhow::{Context as _, ensure};
use common::Application;
use common::request;
use serde_json::{Value, json};
use tokio::time::Instant;
use wamn_test_infrastructure::scratch::ScratchRoot;

const APPLICATION: &str = "platform_fixture";
const SIBLING: &str = "platform_fixture-fixture";
const LIST: &str = "platform-fixture:widget/list@2.1.1";
const ISSUE: &str = "platform-fixture:number/issue@2.1.1";
/// A schedule each minute admits its first run within two minutes.
const SCHEDULE_DEADLINE: Duration = Duration::from_secs(150);
const RUN_DEADLINE: Duration = Duration::from_secs(120);

fn graph(id: &str, entry: &Value, export: &str, respond: bool) -> Value {
    let mut nodes = json!({
        "entry": {"entry": entry},
        "call": {"call": {"sibling": SIBLING, "export": export}},
    });
    let mut edges = vec![json!({"from": "entry", "to": "call"})];
    if respond {
        nodes["done"] = json!({"response": {"status": 200}});
        edges.push(json!({"from": "call", "to": "done"}));
    }
    json!({"format-version": "0.2", "wiring-id": id, "version": 1, "nodes": nodes, "edges": edges})
}

fn issue(key: &str) -> Value {
    json!([{"request_id": key, "value": {"idempotency_key": key, "batch_id": "w"}}])
}

struct Workflows<'a> {
    application: &'a Application,
    scratch: ScratchRoot,
}

impl Workflows<'_> {
    /// Run one `wamn workflow` verb on the application host with the owner session: an
    /// API service cannot hold the admin role that workflow administration needs.
    async fn wamn(&self, args: &[&str]) -> anyhow::Result<String> {
        let mut command = self.application.environment.org.command();
        command.arg("workflow").args(args).args([
            "--host-url",
            &format!("http://{}", self.application.routes.host),
        ]);
        request::output(command).await
    }

    fn file(&self, name: &str, value: &Value) -> anyhow::Result<String> {
        let path = self.scratch.path().join(name);
        std::fs::write(&path, value.to_string())?;
        Ok(path.to_str().context("scratch path")?.to_owned())
    }

    /// Author a document's version 1 and enable it.
    async fn enable(&self, document: &Value, role: Option<&str>) -> anyhow::Result<()> {
        let id = document["wiring-id"].as_str().context("wiring id")?;
        let file = self.file(&format!("{id}.json"), document)?;
        let mut author = vec!["author", "--document", &file];
        if let Some(role) = role {
            author.extend(["--execution-role", role]);
        }
        self.wamn(&author).await?;
        self.wamn(&["enable", "--workflow-id", id, "--version", "1"])
            .await
            .map(drop)
    }

    /// POST a body to a Request workflow's path with the owner session, which holds the
    /// workflow's permission.
    async fn request(&self, path: &str, body: &Value) -> anyhow::Result<(u16, Value)> {
        let routes = &self.application.routes;
        let response = routes
            .client
            .post(format!("{}/{APPLICATION}/flow{path}", routes.endpoint))
            .header("Host", &routes.host)
            .bearer_auth(&self.application.environment.org.token)
            .json(body)
            .send()
            .await?;
        Ok((response.status().as_u16(), response.json().await?))
    }
}

/// The run rows of one workflow, as (trigger source, status).
async fn runs(database: &mut common::Observed, id: &str) -> anyhow::Result<Vec<(String, String)>> {
    Ok(sqlx::query_as(
        "SELECT trigger_source, status FROM wamn_run.runs WHERE wiring_id = $1 ORDER BY created_at",
    )
    .bind(id)
    .fetch_all(&mut database.connection)
    .await?)
}

/// W1 to W5: a Request graph that reads runs inline, one that writes runs durably, Run
/// now admits a run, a minute Schedule admits a run, and a Data change admits a run.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: cluster, bin:helm"]
async fn each_trigger_runs_its_workflow() -> anyhow::Result<()> {
    let application = Application::prepare(&request::fixture(APPLICATION)).await?;
    let workflows = Workflows {
        application: &application,
        scratch: ScratchRoot::create()?,
    };
    // The Schedule and Data change entries give an entry output that no operation
    // export accepts, so their calls refuse; the contract is the admitted run.
    workflows
        .enable(
            &graph(
                "minute",
                &json!({"schedule": {"cron": "* * * * *"}}),
                ISSUE,
                false,
            ),
            Some("admin"),
        )
        .await?;
    let change =
        json!({"data-change": {"package": APPLICATION, "entity": "counter", "ops": ["insert"]}});
    workflows
        .enable(&graph("change", &change, ISSUE, false), Some("admin"))
        .await?;
    workflows
        .enable(
            &graph(
                "inline",
                &json!({"request": {"path": "/inline"}}),
                LIST,
                true,
            ),
            None,
        )
        .await?;
    workflows
        .enable(
            &graph(
                "durable",
                &json!({"request": {"path": "/durable"}}),
                ISSUE,
                true,
            ),
            None,
        )
        .await?;
    let mut database = application.environment.observe().await?;

    let (status, body) = workflows.request("/inline", &json!([{}])).await?;
    ensure!(
        status == 200 && body.is_array(),
        "W1 answers its read: {status} {body}"
    );
    let rows = runs(&mut database, "inline").await?;
    ensure!(
        rows.is_empty(),
        "W1 runs inline and writes no run row: {rows:?}"
    );

    let (status, body) = workflows.request("/durable", &issue("w2")).await?;
    ensure!(
        status == 200 && body[0]["value"]["bare"].is_string(),
        "W2 answers the command's result: {status} {body}"
    );
    let rows = runs(&mut database, "durable").await?;
    ensure!(
        rows.iter()
            .map(|(source, _)| source.as_str())
            .eq(["request"]),
        "W2 runs durably with one request run row: {rows:?}"
    );

    let input = workflows.file("run-now.json", &issue("w3"))?;
    let run = workflows
        .wamn(&[
            "run",
            "--workflow-id",
            "durable",
            "--run-as",
            "admin",
            "--idempotency-key",
            "w3",
            "--input",
            &input,
        ])
        .await?;
    let run = run.trim();
    let end = Instant::now() + RUN_DEADLINE;
    let (source, status) = loop {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT trigger_source, status FROM wamn_run.runs WHERE run_id::text = $1",
        )
        .bind(run)
        .fetch_optional(&mut database.connection)
        .await?;
        match row {
            Some((_, status)) if status == "running" || status == "dispatched" => {}
            Some(row) => break row,
            None => {}
        }
        ensure!(
            Instant::now() < end,
            "W3 run {run} ends within {RUN_DEADLINE:?}"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    ensure!(
        source == "manual" && status == "completed",
        "W3 run {run}: {source} {status}"
    );

    let key = format!("w5-{}", uuid::Uuid::new_v4().simple());
    application
        .routes
        .post(
            "/counter/create",
            json!({"request_id": key, "idempotency_key": key, "label": key}),
        )
        .await?;
    for (id, source, deadline) in [
        ("change", "event", RUN_DEADLINE),
        ("minute", "schedule", SCHEDULE_DEADLINE),
    ] {
        let end = Instant::now() + deadline;
        while !runs(&mut database, id)
            .await?
            .iter()
            .any(|(found, _)| found == source)
        {
            ensure!(
                Instant::now() < end,
                "{id} admits a {source} run within {deadline:?}"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    Ok(())
}
