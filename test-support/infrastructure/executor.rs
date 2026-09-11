//! Run the real idle executor and assert its native probes and signal shutdown.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::Read as _;
use std::net::TcpListener;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use base64::Engine as _;
use rustix::process::{Pid, Signal, kill_process};
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::time::{sleep, timeout};
use wamn_control_registry::Triple;

use crate::event_broker::Credentials;

/// The existing release and private credentials selected by the app test.
#[derive(Debug)]
pub struct ExecutorInput<'a> {
    pub binary: &'a Path,
    pub host_secrets: &'a Path,
    pub component_artifact_base: &'a str,
    pub release_artifact_base: &'a str,
    pub manifest_digest: &'a str,
    pub registry_auth: &'a Path,
    pub nats_url: &'a str,
    pub event_scope: &'a Triple,
    pub project: &'a str,
    pub schema: &'a str,
    pub credentials: &'a Credentials,
    pub source: &'a str,
    pub stream: &'a async_nats::jetstream::stream::Config,
}

fn read_json(path: &Path) -> anyhow::Result<Value> {
    serde_json::from_slice(&std::fs::read(path)?).context("read private executor input")
}

/// Decode a database password for the test log redactor.
pub fn decoded_password(value: &str) -> anyhow::Result<String> {
    let mut input = value.bytes();
    let mut output = Vec::new();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let pair = [
                input
                    .next()
                    .context("password escape lacks its first digit")?,
                input
                    .next()
                    .context("password escape lacks its second digit")?,
            ];
            output.extend(hex::decode(pair).context("password escape has invalid digits")?);
        } else {
            output.push(byte);
        }
    }
    String::from_utf8(output).context("password text is invalid")
}

fn secret_texts(
    urls: &[(&str, String)],
    registry: &Value,
    event_password: &str,
) -> anyhow::Result<Vec<String>> {
    let mut secrets = BTreeSet::from([event_password.to_owned()]);
    for (_, value) in urls {
        secrets.insert(value.clone());
        let parsed = url::Url::parse(value)
            .map_err(|_| anyhow::anyhow!("executor database URL is invalid"))?;
        if let Some(password) = parsed.password() {
            secrets.insert(password.to_owned());
            secrets.insert(decoded_password(password)?);
        }
    }
    for entry in registry["auths"]
        .as_object()
        .context("registry credentials lack auths")?
        .values()
    {
        for key in ["auth", "password", "identitytoken", "registrytoken"] {
            if let Some(value) = entry[key].as_str() {
                secrets.insert(value.to_owned());
            }
        }
        if let Some(value) = entry["auth"].as_str() {
            let pair = String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(value)
                    .context("decode registry credentials")?,
            )?;
            let (_, password) = pair
                .split_once(':')
                .context("registry credentials lack a password")?;
            secrets.insert(password.to_owned());
            secrets.insert(pair);
        }
    }
    let mut secrets = secrets
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
    Ok(secrets)
}

fn redact(mut text: String, secrets: &[String]) -> String {
    for secret in secrets {
        text = text.replace(secret, "<redacted>");
    }
    text
}

fn file_hash(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex::encode(digest.finish().as_ref()))
}

async fn probe(
    client: &reqwest::Client,
    port: u16,
    path: &str,
    secrets: &[String],
) -> anyhow::Result<Value> {
    let started = Instant::now();
    let mut response = client
        .get(format!("http://127.0.0.1:{port}{path}"))
        .send()
        .await?;
    let status = response.status().as_u16();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            body.len() + chunk.len() <= 4096,
            "native probe response exceeds 4096 bytes"
        );
        body.extend_from_slice(&chunk);
    }
    Ok(
        json!({"path":path,"status":status,"body":redact(String::from_utf8_lossy(&body).into_owned(),secrets),"elapsed_seconds":started.elapsed().as_secs_f64()}),
    )
}

/// Assert readiness, liveness, and clean SIGTERM and SIGINT exits for the real executor.
pub async fn assert_idle_lifecycle(
    input: &ExecutorInput<'_>,
    evidence: &Path,
) -> anyhow::Result<()> {
    let mut report = json!({"source":input.source,"manifest_digest":input.manifest_digest,"binary":input.binary,
        "scope":"Real idle executor probes and signal shutdown. Queued delivery is outside this test.",
        "startup_budget_seconds":45,"deployment_grace_seconds":15,"verdict":"fail","cases":[]});
    let mut secrets = Vec::new();
    let result: anyhow::Result<()> = async {
        let mut urls = Vec::new();
        for (name, stem) in [
            ("WAMN_PG_URL", "guest-sql"),
            ("WAMN_EXECUTOR_PLATFORM_PG_URL", "executor-platform"),
            ("WAMN_HTTP_ADMITTER_PG_URL", "http-admitter"),
        ] {
            let document = read_json(&input.host_secrets.join(format!("{stem}.json")))?;
            let value = document["stringData"]["url"]
                .as_str()
                .filter(|value| !value.is_empty())
                .context("missing provisioned executor credential")?;
            urls.push((name, value.to_owned()));
        }
        secrets = secret_texts(
            &urls,
            &read_json(input.registry_auth)?,
            &std::fs::read_to_string(&input.credentials.password_file)?,
        )?;
        report["binary_sha256"] = json!(file_hash(input.binary)?);
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(1))
            .build()?;
        for (name, signal) in [("SIGTERM", Signal::TERM), ("SIGINT", Signal::INT)] {
            let started = Instant::now();
            let mut case =
                json!({"signal":name,"verdict":"fail","ready_attempts":0,"cleanup_killed":false});
            let listener = TcpListener::bind(("127.0.0.1", 0))?;
            let port = listener.local_addr()?.port();
            drop(listener);
            let address = format!("127.0.0.1:{port}");
            case["probe_bind"] = json!(address);
            let private_path = input.host_secrets.join(format!("executor-{name}.raw"));
            let private_log = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&private_path)?;
            let mut command = Command::new(input.binary);
            command
                .env_clear()
                .env(
                    "PATH",
                    std::env::var_os("PATH").context("executor requires PATH")?,
                )
                .envs(urls.iter().map(|(key, value)| (*key, value)))
                .env("WAMN_EVT_NATS_URL", input.nats_url)
                .env("WAMN_EVT_ORG", &input.event_scope.org)
                .env("WAMN_EVT_PROJECT", &input.event_scope.project)
                .env("WAMN_EVT_ENV", input.event_scope.env.as_str())
                .env("WAMN_EVT_NATS_USERNAME", &input.credentials.username)
                .env(
                    "WAMN_EVT_NATS_PASSWORD_FILE",
                    &input.credentials.password_file,
                )
                .env(
                    "WAMN_EVT_STREAM_REPLICAS",
                    input.stream.num_replicas.to_string(),
                )
                .env(
                    "WAMN_EVT_DUP_WINDOW_SECS",
                    input.stream.duplicate_window.as_secs().to_string(),
                )
                .args([
                    "--project",
                    input.project,
                    "--schema",
                    input.schema,
                    "--runner",
                    &format!("cutover-{}", name.to_ascii_lowercase()),
                    "--component-artifact-base",
                    input.component_artifact_base,
                    "--release-artifact-base",
                    input.release_artifact_base,
                    "--release-manifest-digest",
                    input.manifest_digest,
                    "--registry-auth-file",
                ])
                .arg(input.registry_auth)
                .args(["--allow-insecure-registries", "--readiness-bind", &address])
                .stdin(Stdio::null())
                .stdout(private_log.try_clone()?)
                .stderr(private_log)
                .process_group(0)
                .kill_on_drop(true);
            let mut child = command.spawn().context("start the idle executor")?;
            let pid = child.id().context("executor has no process id")?;
            case["pid"] = json!(pid);
            let outcome: anyhow::Result<()> = async {
                let deadline = Instant::now() + Duration::from_secs(45);
                let mut attempts = 0;
                loop {
                    ensure!(
                        child.try_wait()?.is_none(),
                        "executor exited before native readiness"
                    );
                    attempts += 1;
                    case["ready_attempts"] = json!(attempts);
                    if let Ok(response) = probe(&http, port, "/readyz", &secrets).await {
                        let ready = response["status"] == 200;
                        case["ready"] = response;
                        if ready {
                            break;
                        }
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "executor did not reach native readiness within 45 seconds"
                    );
                    sleep(Duration::from_millis(50)).await;
                }
                case["ready_seconds"] = json!(started.elapsed().as_secs_f64());
                case["live"] = probe(&http, port, "/livez", &secrets).await?;
                ensure!(case["live"]["status"] == 200, "ready executor is not live");
                ensure!(
                    child.try_wait()?.is_none(),
                    "executor exited before its shutdown signal"
                );
                let shutdown = Instant::now();
                kill_process(
                    Pid::from_raw(i32::try_from(pid)?).context("executor process id is invalid")?,
                    signal,
                )?;
                case["signal_sent"] = json!(true);
                let exited = timeout(Duration::from_secs(15), child.wait()).await;
                case["shutdown_seconds"] = json!(shutdown.elapsed().as_secs_f64());
                let status =
                    exited.context("executor exceeded its 15-second deployment grace")??;
                case["exit_code"] = json!(status.code());
                ensure!(status.success(), "executor signal shutdown failed");
                ensure!(
                    shutdown.elapsed() < Duration::from_secs(15),
                    "executor exceeded its deployment grace"
                );
                case["verdict"] = json!("pass");
                Ok(())
            }
            .await;
            let cleanup: anyhow::Result<()> = async {
                if child.try_wait()?.is_none() {
                    case["cleanup_killed"] = json!(true);
                    child.start_kill()?;
                }
                let status = timeout(Duration::from_secs(5), child.wait())
                    .await
                    .context("executor cleanup exceeded five seconds")??;
                case["exit_code"] = json!(status.code());
                Ok(())
            }
            .await;
            let log_name = format!("executor-{name}.log");
            let raw = std::fs::read(&private_path)?;
            std::fs::write(
                evidence.join(&log_name),
                redact(String::from_utf8_lossy(&raw).into_owned(), &secrets),
            )?;
            std::fs::remove_file(private_path)?;
            case["log"] = json!(log_name);
            case["case_seconds"] = json!(started.elapsed().as_secs_f64());
            report["cases"]
                .as_array_mut()
                .expect("cases is an array")
                .push(case);
            cleanup?;
            outcome?;
        }
        Ok(())
    }
    .await;
    if let Err(error) = &result {
        report["failure"] = json!(redact(format!("{error:#}"), &secrets));
    } else {
        report["verdict"] = json!("pass");
    }
    std::fs::write(
        evidence.join("executor-lifecycle.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_redaction_covers_database_registry_and_event_credentials() {
        let urls = [(
            "WAMN_PG_URL",
            "postgresql://user:db%2Bpass+word@localhost/db".to_owned(),
        )];
        let registry = json!({"auths":{"registry":{"auth":"dXNlcjpyZWdpc3RyeS1wYXNz","identitytoken":"private-token"}}});
        let values = secret_texts(&urls, &registry, "event-password").unwrap();
        let text = format!(
            "{} db+pass+word db%2Bpass+word user:registry-pass registry-pass private-token event-password",
            urls[0].1
        );
        assert_eq!(
            redact(text, &values),
            "<redacted> <redacted> <redacted> <redacted> <redacted> <redacted> <redacted>"
        );
    }
}
