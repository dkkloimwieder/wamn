//! Connection dispatch and caller refusal through the deployed product path.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use sqlx::Row as _;
use wamn_environment::environment::document::EnvironmentDocument;

use super::{Journey, get, observer};

pub(super) async fn exercise(journey: &Journey<'_>, release: &str) -> anyhow::Result<()> {
    journey
        .apply(
            "connection-environment",
            journey
                .command(&["env", "apply"])
                .arg(&journey.file)
                .args(["--application", "alpha"]),
        )
        .await?;
    journey
        .apply(
            "connection-schema",
            journey
                .command(&["schema", "apply", "alpha", "--release", release, "--env"])
                .arg(&journey.file),
        )
        .await?;

    // The installed web service supplies the upstream. Its health endpoint
    // needs no application credentials or extra fixture workload.
    let service = get(journey.run, "hosts", "service", "wamn-edge").await?;
    let port = service["spec"]["ports"]
        .as_array()
        .context("web service ports")?
        .iter()
        .find(|port| port["name"] == "http")
        .context("web HTTP port")?["port"]
        .as_u64()
        .context("web HTTP port number")?;
    let configuration = journey.run.directory.join("connection.json");
    std::fs::write(
        &configuration,
        serde_json::to_vec(&json!({
            "project":journey.project,"name":"upstream","integration_type":"http",
            "auth_type":"http_basic","external_system":null
        }))?,
    )?;
    journey
        .apply(
            "upstream-integration",
            journey
                .command(&["integration", "write", "--file"])
                .arg(&configuration),
        )
        .await?;
    std::fs::write(
        &configuration,
        serde_json::to_vec(&json!({
            "project":journey.project,"env":journey.environment,"name":"upstream",
            "parameters":{"primary-authority":format!("http://wamn-edge.hosts.svc.cluster.local:{port}"),
                "tls-verification":"disabled"}
        }))?,
    )?;
    let credential = journey.run.directory.join("connection-credential.json");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&credential)?;
    file.write_all(&serde_json::to_vec(
        &json!({"username":"fixture","password":"fixture-connection-key"}),
    )?)?;
    drop(file);
    journey
        .apply(
            "connection-write",
            journey
                .command(&["connection", "write", "--file"])
                .arg(&configuration)
                .arg("--credential-file")
                .arg(&credential),
        )
        .await?;
    std::fs::remove_file(credential)?;
    std::fs::write(
        &configuration,
        serde_json::to_vec(&json!({
            "project":journey.project,"env":journey.environment,"name":"upstream"
        }))?,
    )?;
    journey
        .apply(
            "connection-enable",
            journey
                .command(&["connection", "enable", "--file"])
                .arg(&configuration),
        )
        .await?;
    let mut document = EnvironmentDocument::compile(&journey.file)?;
    document.applications.retain(|name, _| name == "alpha");
    document
        .applications
        .get_mut("alpha")
        .context("alpha application")?
        .connections = BTreeMap::from([("upstream".to_owned(), "upstream".to_owned())]);
    std::fs::write(&journey.file, document.to_kcl())?;
    journey
        .apply(
            "connection-bindings",
            journey
                .command(&["env", "apply"])
                .arg(&journey.file)
                .args(["--application", "alpha"]),
        )
        .await?;
    journey
        .apply("connection-deploy", &mut journey.deploy("alpha", release))
        .await?;

    let coordinate = format!("{}/{}/{}", document.org, document.env, document.project);
    let input = journey.run.directory.join("connection-input.json");
    std::fs::write(&input, serde_json::to_vec(&json!({"id":1}))?)?;
    let mut database = observer(journey).await?;
    let application_url = format!("{}/api", journey.edge.trim_end_matches('/'));
    for (workflow, terminal) in [
        ("connection-without-response", false),
        ("response-without-caller", true),
    ] {
        let mut graph = json!({
            "format-version":"0.1","wiring-id":workflow,"version":1,"entry":"input",
            "nodes":{
                "input":{"component":"transform","interface-version":"0.1.0","operation":"wamn:node/handler@0.1.0","params":{"pointer":""}},
                "send":{"component":"http-request","interface-version":"0.1.0","operation":"wamn:node/async-handler@0.1.0",
                    "params":{"requirement":"upstream","method":"GET","path-and-query":"/healthz"}}
            },
            "edges":[{"from":"input","to":"send"}]
        });
        if terminal {
            graph["nodes"]["send"]["terminal"] = "respond".into();
        }
        let file = journey.run.directory.join(format!("{workflow}.json"));
        std::fs::write(&file, serde_json::to_vec(&graph)?)?;
        journey
            .apply(
                &format!("author-{workflow}"),
                journey
                    .command(&[
                        "workflow",
                        "author",
                        "--coordinate",
                        &coordinate,
                        "--application",
                        "alpha",
                        "--execution-type",
                        "invoker",
                        "--document",
                    ])
                    .arg(&file),
            )
            .await?;
        journey
            .apply(
                &format!("enable-{workflow}"),
                &mut journey.command(&[
                    "workflow",
                    "enable",
                    "--coordinate",
                    &coordinate,
                    "--application",
                    "alpha",
                    "--workflow-id",
                    workflow,
                    "--version",
                    "1",
                ]),
            )
            .await?;
        let output = journey
            .run(
                &format!("start-{workflow}"),
                journey
                    .command(&[
                        "workflow",
                        "start",
                        "--host-url",
                        &application_url,
                        "--application",
                        "alpha",
                        "--workflow-id",
                        workflow,
                        "--idempotency-key",
                        workflow,
                        "--input",
                    ])
                    .arg(&input)
                    .env("WAMN_TOKEN", journey.token),
            )
            .await?;
        ensure!(
            output.status.success(),
            "workflow start refused: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let run = String::from_utf8(output.stdout)?;
        let (status, body) = tokio::time::timeout(Duration::from_secs(90), async {
            loop {
                let row =
                    sqlx::query("SELECT status, result_json FROM wamn_run.runs WHERE run_id=$1")
                        .bind(run.trim())
                        .fetch_one(&mut database)
                        .await?;
                let status: String = row.get(0);
                if matches!(
                    status.as_str(),
                    "completed" | "failed" | "infrastructure-failure"
                ) {
                    return Ok::<_, anyhow::Error>((
                        status,
                        row.get::<Option<Value>, _>(1).unwrap_or(Value::Null),
                    ));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .context("connection workflow timed out")??;
        if terminal {
            ensure!(
                status == "failed" && body["error"]["code"] == "respond-without-caller",
                "response without caller must refuse: {status}: {body}"
            );
        } else {
            ensure!(
                status == "completed" && body["status"] == 200,
                "deployed connection must reach its upstream: {status}: {body}"
            );
        }
    }
    Ok(())
}
