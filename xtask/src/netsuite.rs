//! Operator calls through the deployed NetSuite integration and workflows.

use anyhow::{Context as _, bail, ensure};
use clap::Args as ClapArgs;
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

#[derive(Debug, ClapArgs)]
pub struct Args {
    credentials: PathBuf,
    project: String,
    env: String,
    application: String,
    host_url: String,
    record_id: String,
    query_file: PathBuf,
}

#[derive(Deserialize)]
struct Credentials {
    account_id: String,
    client_id: String,
    certificate_id: String,
    algorithm: String,
    private_key_file: PathBuf,
}

fn write(path: &Path, value: &Value) -> anyhow::Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    serde_json::to_writer(file, value)?;
    Ok(())
}

async fn cli(program: &std::ffi::OsStr, args: &[&std::ffi::OsStr]) -> anyhow::Result<String> {
    let output = Command::new(program)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await?;
    // A failed CLI can echo request data. Do not copy its output into logs.
    ensure!(
        output.status.success(),
        "NetSuite CLI call failed: {}",
        output.status
    );
    String::from_utf8(output.stdout).context("CLI output is not UTF-8")
}

pub async fn run(args: &Args) -> anyhow::Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = execute(args) => result,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            bail!("NetSuite calls interrupted. Inspect the runs before another call.");
        }
        _ = terminate.recv() => bail!("NetSuite calls terminated. Inspect the runs before another call."),
    }
}

async fn execute(args: &Args) -> anyhow::Result<()> {
    for name in ["WAMN_CONTROL_URL", "WAMN_TOKEN", "WAMN_PG_ADMIN_URL"] {
        ensure!(
            std::env::var_os(name).is_some_and(|value| !value.is_empty()),
            "set {name}"
        );
    }
    let credentials: Credentials = serde_json::from_slice(&std::fs::read(&args.credentials)?)
        .context("read NetSuite credentials")?;
    ensure!(
        !credentials.account_id.is_empty()
            && credentials
                .account_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && !credentials.client_id.is_empty()
            && !credentials.certificate_id.is_empty()
            && !credentials.private_key_file.as_os_str().is_empty()
            && matches!(credentials.algorithm.as_str(), "PS256" | "ES256"),
        "invalid NetSuite credentials"
    );
    // Resolve the key from the caller's directory, as the operator command does.
    let private_key = std::fs::read_to_string(&credentials.private_key_file)?;
    let query = std::fs::read_to_string(&args.query_file)?;
    let temporary = std::env::var_os("TMPDIR")
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from("/var/tmp"), PathBuf::from);
    let work = tempfile::Builder::new()
        .prefix("wamn-netsuite-live.")
        .tempdir_in(temporary)?;
    let integration = work.path().join("integration.json");
    let connection = work.path().join("connection.json");
    let credential = work.path().join("credential.json");
    let origin = format!(
        "https://{}.suitetalk.api.netsuite.com",
        credentials.account_id
    );
    write(
        &integration,
        &json!({
            "project":args.project, "name":"netsuite", "integration_type":"http",
            "auth_type":"oauth2_client_credentials_jwt"
        }),
    )?;
    write(
        &connection,
        &json!({
            "project":args.project, "env":args.env, "name":"netsuite", "parameters":{
                "primary-authority":origin, "tls-verification":"verify-authority", "auth":{
                    "client_id":credentials.client_id,
                    "token_url":format!("{origin}/services/rest/auth/oauth2/v1/token"),
                    "scopes":["rest_webservices"], "key_id":credentials.certificate_id,
                    "algorithm":credentials.algorithm, "scope_separator":","
                }
            }
        }),
    )?;
    write(&credential, &json!({"private_key":private_key}))?;
    let program = std::env::var_os("WAMN_CTL")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "wamn".into());
    cli(
        &program,
        &[
            "integration".as_ref(),
            "write".as_ref(),
            "--file".as_ref(),
            integration.as_os_str(),
        ],
    )
    .await?;
    cli(
        &program,
        &[
            "connection".as_ref(),
            "write".as_ref(),
            "--file".as_ref(),
            connection.as_os_str(),
            "--credential-file".as_ref(),
            credential.as_os_str(),
        ],
    )
    .await?;
    let mut database = wamn_postgres::connect(&std::env::var("WAMN_PG_ADMIN_URL")?).await?;
    for (workflow, input) in [
        ("record_get", json!({"id":args.record_id})),
        ("suiteql_query", json!({"q":query})),
    ] {
        let input_file = work.path().join(format!("{workflow}.json"));
        write(&input_file, &input)?;
        let key = format!(
            "netsuite-live:{}:{workflow}",
            work.path()
                .file_name()
                .context("temporary directory name")?
                .to_string_lossy()
        );
        let run = cli(
            &program,
            &[
                "workflow".as_ref(),
                "start".as_ref(),
                "--host-url".as_ref(),
                args.host_url.as_ref(),
                "--application".as_ref(),
                args.application.as_ref(),
                "--workflow-id".as_ref(),
                workflow.as_ref(),
                "--idempotency-key".as_ref(),
                key.as_ref(),
                "--input".as_ref(),
                input_file.as_os_str(),
            ],
        )
        .await?;
        let run = run.trim();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        loop {
            let result: Value = tokio::time::timeout_at(deadline, sqlx::query_scalar(
                "SELECT jsonb_build_object('run_id',run_id,'status',status,'result',result_json,'reason',terminal_reason) FROM wamn_run.runs WHERE run_id=$1"
            ).bind(run).fetch_one(&mut database)).await
                .with_context(|| format!("Run {run} did not finish within 120 seconds. Inspect it before another call."))??;
            match result["status"].as_str().context("run has no status")? {
                "completed" => {
                    println!("{workflow}: {result}");
                    break;
                }
                "failed" | "infrastructure-failure" | "cancelled" | "expired" => {
                    bail!("{workflow}: {result}")
                }
                _ => {}
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "Run {run} did not finish within 120 seconds. Inspect it before another call."
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    Ok(())
}
